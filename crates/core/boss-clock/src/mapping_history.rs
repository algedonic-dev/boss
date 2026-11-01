//! Original clock coverage. Capture precedes commit; audit insertion
//! follows it. The interval between them is uncertainty, never an
//! invented activation timestamp. A later full original can recover
//! coverage without laundering the earlier gap.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ClockMode, SimClockParams};

const ACTIVATION_UNCERTAINTY: &str =
    "clock activation is bounded by producer capture and audit insertion";
const INITIAL_UNCERTAINTY: &str = "initial original clock coverage is missing";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClockSpan {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub revision: String,
    pub epoch: String,
    pub mode: ClockMode,
    pub params: Option<SimClockParams>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnknownClockSpan {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockTimeline {
    pub known: Vec<ClockSpan>,
    pub unknown: Vec<UnknownClockSpan>,
    /// Already-collected native originals, not inferred from the reduced
    /// known/unknown spans. Missing on old checkpoints means no witness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome_originals: Option<OutcomeOriginals>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutcomeOriginals {
    facts: Vec<ClockMappingFact>,
    native_head: Option<String>,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
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
                outcome_originals: None,
            };
        }
        let mut known = Vec::new();
        let mut barriers = Vec::new();
        for (index, fact) in facts.iter().enumerate() {
            barriers.push(UnknownClockSpan {
                since: fact.captured_at,
                until: fact.audit_created_at,
                reason: ACTIVATION_UNCERTAINTY.into(),
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
                .map_or(INITIAL_UNCERTAINTY, |s| s.reason.as_str());
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
        Self {
            known,
            unknown,
            outcome_originals: Some(OutcomeOriginals {
                facts: facts.to_vec(),
                native_head: native_head.map(str::to_string),
                since,
                until,
            }),
        }
    }

    fn wall_outcome_originals(&self) -> Result<&OutcomeOriginals, String> {
        let original = self
            .outcome_originals
            .as_ref()
            .ok_or("original clock candidate roster unavailable")?;
        let first = original
            .facts
            .first()
            .ok_or("original clock candidate roster is empty")?;
        let last = original
            .facts
            .last()
            .ok_or("original clock candidate roster is empty")?;
        let unique: std::collections::BTreeSet<_> =
            original.facts.iter().map(|f| &f.revision).collect();
        if original.since >= original.until
            || unique.len() != original.facts.len()
            || first.previous_revision.is_some()
            || original.native_head.as_deref() != Some(last.revision.as_str())
            || original.facts.iter().any(|f| {
                f.refusal().is_some()
                    || f.mode != Some(ClockMode::Wall)
                    || f.params.is_some()
                    || f.seq <= 0
                    || f.revision.is_empty()
                    || f.captured_at > f.audit_created_at
                    || f.audit_created_at > original.until
            })
            || original.facts.windows(2).any(|p| {
                p[1].previous_revision.as_deref() != Some(p[0].revision.as_str())
                    || p[1].seq <= p[0].seq
                    || p[1].captured_at < p[0].audit_created_at
            })
        {
            return Err(
                "complete nonoverlapping original WALL candidate roster unavailable".into(),
            );
        }
        // This restricted roster has disjoint activation intervals. Validate
        // its exact reduced spans in one pass, not by re-running the general
        // timeline builder for each packet/partition.
        let mut known = Vec::new();
        let mut unknown: Vec<UnknownClockSpan> = Vec::new();
        let mut gap = |since: DateTime<Utc>, until: DateTime<Utc>, reason: &str| {
            let since = since.max(original.since);
            let until = until.min(original.until);
            if since < until {
                if let Some(last) = unknown.last_mut()
                    && last.until == since
                    && last.reason == reason
                {
                    last.until = until;
                } else {
                    unknown.push(UnknownClockSpan {
                        since,
                        until,
                        reason: reason.into(),
                    });
                }
            }
        };
        gap(original.since, first.captured_at, INITIAL_UNCERTAINTY);
        for (index, fact) in original.facts.iter().enumerate() {
            gap(
                fact.captured_at,
                fact.audit_created_at,
                ACTIVATION_UNCERTAINTY,
            );
            let since = fact.audit_created_at.max(original.since);
            let until = original
                .facts
                .get(index + 1)
                .map_or(original.until, |f| f.captured_at)
                .min(original.until);
            if since < until {
                let (Some(epoch), Some(mode)) = (&fact.epoch, fact.mode) else {
                    return Err("original candidate mode/epoch unavailable".into());
                };
                known.push(ClockSpan {
                    since,
                    until,
                    revision: fact.revision.clone(),
                    epoch: epoch.clone(),
                    mode,
                    params: fact.params,
                });
            }
        }
        if known != self.known || unknown != self.unknown {
            return Err("clock spans differ from their conserved original roster".into());
        }
        Ok(original)
    }

    /// Bounds on candidate dates for an outcome proof, NEVER known activation
    /// or date_at coverage (design16fad Q1/Q2). The first conservative component
    /// accepts only a complete original WALL chain; all other formulas refuse.
    pub fn outcome_date_bounds(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        audit_cutoff: i64,
    ) -> Result<(NaiveDate, NaiveDate), String> {
        let original = self.wall_outcome_originals()?;
        let first = original
            .facts
            .first()
            .ok_or("original initial model unavailable")?;
        if since >= until
            || since < original.since.max(first.audit_created_at)
            || until > original.until
            || original.facts.iter().any(|f| f.seq > audit_cutoff)
        {
            return Err("outcome partition is outside its original clock/audit cut".into());
        }
        let last = until
            .checked_sub_signed(chrono::Duration::microseconds(1))
            .ok_or("outcome partition exceeds supported time")?;
        if last < since {
            return Err("outcome partition is below original microsecond precision".into());
        }
        // Evaluate the shared original formula, without inserting this
        // candidate into known coverage or selecting an activation instant.
        let (Some(epoch), Some(mode)) = (&first.epoch, first.mode) else {
            return Err("original candidate mode/epoch unavailable".into());
        };
        let candidate = ClockSpan {
            since,
            until,
            revision: first.revision.clone(),
            epoch: epoch.clone(),
            mode,
            params: first.params,
        };
        Ok((candidate.date_at(since)?, candidate.date_at(last)?))
    }

    /// Fingerprint only the conserved prefix when comparing checkpoints. Later
    /// complete originals must not rewrite an already witnessed prefix merely
    /// by changing the current native-head pointer. This is not a new head.
    pub fn outcome_evidence_prefix(
        &self,
        until: DateTime<Utc>,
    ) -> Option<(bool, Vec<ClockMappingFact>)> {
        self.outcome_originals.as_ref().map(|original| {
            (
                self.wall_outcome_originals().is_ok(),
                original
                    .facts
                    .iter()
                    .filter(|f| f.captured_at < until)
                    .cloned()
                    .collect(),
            )
        })
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
