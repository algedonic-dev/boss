//! Original clock coverage. Capture precedes commit; audit insertion
//! follows it. The interval between them is uncertainty, never an
//! invented activation timestamp. A later full original can recover
//! coverage without laundering the earlier gap.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ClockMode, SimClockParams};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockMappingFact {
    pub schema_version: u32,
    pub revision: String,
    pub previous_revision: Option<String>,
    pub epoch: Option<String>,
    pub mode: Option<ClockMode>,
    pub params: Option<SimClockParams>,
    pub captured_at: DateTime<Utc>,
    pub audit_created_at: DateTime<Utc>,
    pub seq: i64,
    pub complete_group: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockSpan {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub revision: String,
    pub epoch: String,
    pub mode: ClockMode,
    pub params: Option<SimClockParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownClockSpan {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockTimeline {
    pub known: Vec<ClockSpan>,
    pub unknown: Vec<UnknownClockSpan>,
}

impl ClockMappingFact {
    fn refusal(&self) -> Option<&'static str> {
        if self.schema_version != 1 {
            Some("unsupported original clock schema")
        } else if !self.complete_group {
            Some("original clock transaction group is incomplete")
        } else if self.mode.is_none() || self.epoch.as_deref().is_none_or(str::is_empty) {
            Some("original clock mode or epoch witness is missing")
        } else if self.mode == Some(ClockMode::Sim) && self.params.is_none() {
            Some("original simulated-clock parameters are missing")
        } else {
            None
        }
    }
}

impl ClockSpan {
    fn date_at(&self, wall: DateTime<Utc>) -> Result<NaiveDate, String> {
        if self.mode == ClockMode::Wall {
            return Ok(wall.date_naive());
        }
        let params = self
            .params
            .as_ref()
            .ok_or("original clock parameters missing")?;
        if !params.warp_factor.is_finite()
            || params.warp_factor <= 0.0
            || !params.paused_offset_seconds.is_finite()
            || params.paused_offset_seconds < 0.0
            || (params.paused && params.paused_at.is_none())
        {
            return Err("invalid original clock formula".into());
        }
        let reference = if params.paused {
            params.paused_at.unwrap_or(wall)
        } else {
            wall
        };
        let elapsed = (reference - params.wall_anchor).num_milliseconds().max(0) as f64 / 1000.0;
        let millis =
            (elapsed - params.paused_offset_seconds).max(0.0) * params.warp_factor * 1000.0;
        let epoch = params
            .epoch_start
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid clock epoch")?
            .and_utc();
        if !millis.is_finite()
            || millis >= i64::MAX as f64
            || epoch
                .checked_add_signed(chrono::Duration::milliseconds(millis as i64))
                .is_none()
        {
            return Err("original clock formula exceeds supported date range".into());
        }
        // The date definition itself is shared with actual live reads.
        Ok(params.at(wall).now.date_naive())
    }
}

impl ClockTimeline {
    pub fn build(
        facts: &[ClockMappingFact],
        native_head: Option<&str>,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Self {
        let unique: std::collections::BTreeSet<_> = facts.iter().map(|f| &f.revision).collect();
        let invalid_order = unique.len() != facts.len()
            || facts.windows(2).any(|p| {
                p[0].seq >= p[1].seq
                    || p[0].audit_created_at > p[1].audit_created_at
                    || p[0].revision == p[1].revision
            })
            || facts
                .iter()
                .any(|f| f.seq <= 0 || f.revision.is_empty() || f.captured_at > f.audit_created_at);
        if invalid_order || until <= since {
            return Self {
                known: vec![],
                unknown: vec![UnknownClockSpan {
                    since,
                    until,
                    reason: "original clock order, wall instants or bounds are invalid".into(),
                }],
            };
        }
        let mut known = Vec::new();
        let mut barriers = Vec::new();
        for (index, fact) in facts.iter().enumerate() {
            barriers.push(UnknownClockSpan {
                since: fact.captured_at,
                until: fact.audit_created_at,
                reason: "clock activation is bounded by producer capture and audit insertion"
                    .into(),
            });
            let next = facts.get(index + 1);
            let end = next.map_or(until, |f| f.captured_at).min(until);
            let start = fact.audit_created_at.max(since);
            let broken_next =
                next.is_some_and(|n| n.previous_revision.as_deref() != Some(&fact.revision));
            let broken_head = next.is_none() && native_head != Some(fact.revision.as_str());
            let refusal = fact.refusal().or(if broken_next {
                Some("an original clock revision is missing before its successor")
            } else if broken_head {
                Some("native clock head is absent from the conserved audit slice")
            } else {
                None
            });
            if let Some(reason) = refusal {
                barriers.push(UnknownClockSpan {
                    since: start,
                    until: next.map_or(until, |f| f.audit_created_at).min(until),
                    reason: reason.into(),
                });
            } else if start < end
                && let (Some(epoch), Some(mode)) = (&fact.epoch, fact.mode)
            {
                let span = ClockSpan {
                    since: start,
                    until: end,
                    revision: fact.revision.clone(),
                    epoch: epoch.clone(),
                    mode,
                    params: fact.params,
                };
                let validity = span
                    .date_at(start)
                    .and_then(|_| span.date_at(end - chrono::Duration::microseconds(1)));
                match validity {
                    Ok(_) => known.push(span),
                    Err(reason) => barriers.push(UnknownClockSpan {
                        since: start,
                        until: end,
                        reason,
                    }),
                }
            }
        }
        let mut boundaries = vec![since, until];
        for span in &known {
            boundaries.extend([span.since, span.until]);
        }
        for span in &barriers {
            boundaries.extend([
                span.since.max(since).min(until),
                span.until.max(since).min(until),
            ]);
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut unknown: Vec<UnknownClockSpan> = Vec::new();
        for bounds in boundaries.windows(2) {
            if known
                .iter()
                .any(|s| s.since <= bounds[0] && bounds[0] < s.until)
            {
                continue;
            }
            let reason = barriers
                .iter()
                .rev()
                .find(|s| s.since <= bounds[0] && bounds[0] < s.until)
                .map_or("initial original clock coverage is missing", |s| {
                    s.reason.as_str()
                });
            if let Some(last) = unknown.last_mut()
                && last.until == bounds[0]
                && last.reason == reason
            {
                last.until = bounds[1];
            } else {
                unknown.push(UnknownClockSpan {
                    since: bounds[0],
                    until: bounds[1],
                    reason: reason.into(),
                });
            }
        }
        Self { known, unknown }
    }

    pub fn date_at(&self, wall: DateTime<Utc>) -> Result<NaiveDate, String> {
        self.known
            .iter()
            .find(|s| s.since <= wall && wall < s.until)
            .ok_or_else(|| {
                self.unknown
                    .iter()
                    .find(|s| s.since <= wall && wall < s.until)
                    .map_or("wall instant outside original clock coverage".into(), |s| {
                        s.reason.clone()
                    })
            })?
            .date_at(wall)
    }
}
