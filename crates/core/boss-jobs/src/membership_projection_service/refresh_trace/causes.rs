//! Original cause context, distinct from a full aggregate MembershipReading.
use super::{BoundedOriginal, ORIGINAL_BYTES_PER_FLIGHT};
use crate::membership_replay::{MembershipFlow, MembershipReading, Unavailable};
use crate::port::JobScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, ser::SerializeSeq};

#[derive(Clone, Serialize, Deserialize)]
pub struct OriginalCauses<S = Vec<StationCauses>> {
    stations: S,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    audit_cutoff: i64,
    denied: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StationCauses {
    station: String,
    scope: JobScope,
    caller_binding: Option<String>,
    unknown: Vec<Unavailable>,
}

#[derive(Serialize)]
struct BorrowedStation<'a> {
    station: &'a str,
    scope: &'a JobScope,
    caller_binding: &'a Option<String>,
    unknown: &'a [Unavailable],
}

struct BorrowedStations<'a>(&'a [MembershipFlow]);
impl Serialize for BorrowedStations<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for station in self.0 {
            sequence.serialize_element(&BorrowedStation {
                station: &station.station,
                scope: &station.scope,
                caller_binding: &station.caller_binding,
                unknown: &station.unknown,
            })?;
        }
        sequence.end()
    }
}

#[derive(Clone, Serialize)]
pub struct CauseProjection {
    pub complete: bool,
    pub total_bytes: usize,
    pub total_bytes_complete: bool,
    pub retained_bytes: usize,
    pub total_stations: usize,
    pub retained_stations: usize,
    pub omitted_stations: usize,
    pub total_spans: usize,
    pub retained_spans: usize,
    pub omitted_spans: usize,
    pub total_causes: usize,
    pub retained_causes: usize,
    pub omitted_causes: usize,
    pub original_causes: Option<OriginalCauses>,
    pub unavailable_reason: Option<&'static str>,
}

impl CauseProjection {
    pub fn capture(reading: &MembershipReading) -> Self {
        // Borrow the original strings/spans; count before allocating any owned
        // diagnostic copy, even when a single reason is larger than the bound.
        let borrowed = OriginalCauses {
            stations: BorrowedStations(&reading.stations),
            since: reading.since,
            until: reading.until,
            audit_cutoff: reading.audit_cutoff,
            denied: reading.denied,
        };
        let mut writer = BoundedOriginal {
            bytes: Vec::new(),
            total: 0,
            bound: ORIGINAL_BYTES_PER_FLIGHT,
        };
        let serialized = serde_json::to_writer(&mut writer, &borrowed);
        let original_causes = if serialized.is_ok() && writer.total <= writer.bound {
            serde_json::from_slice(&writer.bytes).ok()
        } else {
            None
        };
        let complete = original_causes.is_some();
        let stations = reading.stations.len();
        let spans = reading.stations.iter().map(|s| s.unknown.len()).sum();
        let causes = spans + usize::from(reading.denied);
        Self {
            complete,
            total_bytes: writer.total,
            total_bytes_complete: serialized.is_ok(),
            retained_bytes: if complete { writer.total } else { 0 },
            total_stations: stations,
            retained_stations: if complete { stations } else { 0 },
            omitted_stations: if complete { 0 } else { stations },
            total_spans: spans,
            retained_spans: if complete { spans } else { 0 },
            omitted_spans: if complete { 0 } else { spans },
            total_causes: causes,
            retained_causes: if complete { causes } else { 0 },
            omitted_causes: if complete { 0 } else { causes },
            original_causes,
            unavailable_reason: if complete {
                None
            } else if serialized.is_err() {
                Some("original cause projection serialization unavailable")
            } else {
                Some("original causes exceed flight diagnostic byte bound")
            },
        }
    }

    pub fn omit(&mut self) {
        self.complete = false;
        self.retained_bytes = 0;
        self.retained_stations = 0;
        self.retained_spans = 0;
        self.retained_causes = 0;
        self.omitted_stations = self.total_stations;
        self.omitted_spans = self.total_spans;
        self.omitted_causes = self.total_causes;
        self.original_causes = None;
        self.unavailable_reason =
            Some("original causes exceed remaining flight diagnostic byte bound");
    }
}
