//! A read of the latest resident supervision, never a substitute for it.
use crate::cadence::{CadenceRuleRow, LastFiring};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub fn participates(verb: &str) -> bool {
    matches!(verb, "reconcile" | "board" | "run" | "refresh")
}
pub fn unknown(reason: &str) -> Value {
    json!({"contract_version":1,"state":"unknown","reason":reason})
}

pub async fn history(
    repo: &dyn crate::cadence::CadenceRepository,
    rules: &[CadenceRuleRow],
    now: DateTime<Utc>,
    page_size: usize,
    max_pages: usize,
) -> Value {
    let mut reads = Vec::new();
    let mut best: Option<crate::cadence::NewFiring> = None;
    let result: Result<(), String> = async {
        if !(1..=500).contains(&page_size) || max_pages == 0 || max_pages > 1000 {
            return Err("history read budget is invalid".into());
        }
        for rule in rules.iter().filter(|r| participates(&r.verb)) {
            let mut cursor: Option<(DateTime<Utc>, String)> = None;
            let mut finished = false;
            for _ in 0..max_pages {
                let rows = repo.firing_history(&rule.name, Some("gate_queue"), cursor.clone(), page_size)
                    .await.map_err(|error| format!("history read for {} failed: {error}",rule.name))?;
                reads.push(json!({"rule":rule.name,"before":cursor,"rows":rows}));
                if rows.len() > page_size { return Err("history exceeded its requested page bound".into()); }
                let mut previous = cursor.clone();
                for row in &rows {
                    let key = (row.fired_at, row.firing_id.clone());
                    if row.rule_name != rule.name || row.firing_id.is_empty()
                        || row.fired_at > now || previous.as_ref().is_some_and(|p| key >= *p) {
                        return Err("history returned an invalid identity, future timestamp, or nonexclusive order".into());
                    }
                    previous = Some(key);
                    if row.detail.pointer("/observations/gate_queue").is_none() {
                        return Err("history omitted the requested named observation".into());
                    }
                }
                for row in &rows {
                    let Some(observation) = row.detail.pointer("/observations/gate_queue") else {
                        return Err("history omitted the requested named observation".into());
                    };
                    if row.detail.get("rc").and_then(Value::as_i64) == Some(0)
                        && participates(&row.verb)
                        && observation.get("contract_version").and_then(Value::as_u64) == Some(1)
                        && observation.get("state").and_then(Value::as_str) == Some("completed")
                        && observation.get("error") == Some(&Value::Null)
                        && observation.get("report").is_some_and(Value::is_object)
                        && observation.get("source_build").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                        && observation.get("observed_at").and_then(Value::as_str).and_then(crate::gate_reservation::instant)
                            .is_some_and(|at| at >= row.fired_at && at <= now) {
                        if best.as_ref().is_none_or(|b| (&row.fired_at,&row.firing_id,&row.rule_name) > (&b.fired_at,&b.firing_id,&b.rule_name)) {
                            best = Some(row.clone());
                        }
                        finished = true;
                        break;
                    }
                }
                if finished || rows.len() < page_size { finished = true; break; }
                cursor = previous;
            }
            if !finished { return Err(format!("history read budget exhausted for {}; no complete last-success claim",rule.name)); }
        }
        Ok(())
    }.await;
    match result {
        Err(reason) => json!({"state":"unknown","reason":reason,"reads":reads}),
        Ok(()) => match best {
            Some(record) => json!({"state":"observed","record":record,"reads":reads}),
            None => {
                json!({"state":"unknown","reason":"no qualified successful observation was found","reads":reads})
            }
        },
    }
}

pub fn project(
    rules: &[CadenceRuleRow],
    firings: &[(String, LastFiring)],
    readable: bool,
    now: Option<DateTime<Utc>>,
) -> Value {
    if !readable {
        return unknown("current cadence or firing records are unreadable or withheld");
    }
    let Some(now) = now else {
        return unknown("current clock is unavailable");
    };
    let active = |name: &str| {
        rules
            .iter()
            .any(|r| r.name == name && participates(&r.verb))
    };
    let Some((rule, firing)) = firings
        .iter()
        .filter(|(name, _)| active(name))
        .max_by(|a, b| {
            (&a.1.fired_at, &a.1.firing_id, &a.0).cmp(&(&b.1.fired_at, &b.1.firing_id, &b.0))
        })
    else {
        return unknown("no resident supervision firing has been observed");
    };
    let Some(minutes) = rules
        .iter()
        .filter(|r| participates(&r.verb) && r.basis == "wall")
        .filter_map(|r| r.every_minutes.filter(|m| *m > 0))
        .min()
    else {
        return unknown("no active recurring supervision cadence is readable");
    };
    let observation = firing
        .observations
        .as_ref()
        .and_then(|o| o.get("gate_queue"));
    let reason = if firing.fired_at > now {
        Some("latest firing is ahead of the observed clock")
    } else if (now - firing.fired_at).num_seconds() > i64::from(minutes) * 60 {
        Some("latest resident supervision is past its active cadence")
    } else if firing.rc.is_none() {
        Some("latest firing has no recorded outcome")
    } else if !matches!(firing.rc, Some(0 | 75)) {
        Some("latest firing has a failed outcome")
    } else if observation.is_none() {
        Some("latest firing has no gate queue observation")
    } else if !observation.is_some_and(|o| {
        o.get("contract_version").and_then(Value::as_u64) == Some(1)
            && o.get("state").and_then(Value::as_str) == Some("completed")
            && o.get("error").is_none_or(Value::is_null)
            && o.get("report").is_some_and(Value::is_object)
            && o.get("source_build")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            && o.get("observed_at")
                .and_then(Value::as_str)
                .and_then(crate::gate_reservation::instant)
                .is_some_and(|at| at >= firing.fired_at && at <= now)
    }) {
        Some("latest gate queue observation is unknown or malformed")
    } else {
        None
    };
    json!({"contract_version":1,"state":if reason.is_none(){"completed"}else{"unknown"},
        "reason":reason,"rule":rule,"firing_id":firing.firing_id,"fired_at":firing.fired_at,
        "observation":observation})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(s: &str) -> DateTime<Utc> {
        crate::gate_reservation::instant(s).unwrap()
    }
    fn rule(name: &str, verb: &str) -> CadenceRuleRow {
        serde_json::from_value(json!({"name":name,"verb":verb,"basis":"wall","every_minutes":5}))
            .unwrap()
    }
    fn firing(id: &str, time: &str, observation: Option<Value>) -> LastFiring {
        LastFiring {
            firing_id: id.into(),
            fired_at: at(time),
            rc: Some(0),
            board_decision: None,
            observations: observation
                .map(|value| serde_json::Map::from_iter([("gate_queue".into(), value)])),
        }
    }
    fn completed() -> Value {
        json!({"contract_version":1,"state":"completed","observed_at":"2026-10-16T10:00:00Z","source_build":"fixture","report":{"launched":[],"retained":[]}})
    }
    #[test]
    fn newer_unknown_missing_failed_or_malformed_never_borrows_predecessor_success() {
        let rules = [rule("old", "reconcile"), rule("new", "refresh")];
        let old = firing("old", "2026-10-16T10:00:00Z", Some(completed()));
        let now = Some(at("2026-10-16T10:02:00Z"));
        assert_eq!(
            project(&rules, &[("old".into(), old.clone())], true, now)["state"],
            "completed"
        );
        for observation in [
            None,
            Some(json!({"contract_version":1,"state":"unknown"})),
            Some(json!({"contract_version":1,"state":"completed"})),
        ] {
            let new = firing("new", "2026-10-16T10:01:00Z", observation);
            let view = project(
                &rules,
                &[("old".into(), old.clone()), ("new".into(), new)],
                true,
                now,
            );
            assert_eq!(view["state"], "unknown");
            assert_eq!(view["firing_id"], "new");
        }
        let mut idle_board = old.clone();
        idle_board.rc = Some(75);
        assert_eq!(
            project(&rules, &[("old".into(), idle_board)], true, now)["state"],
            "completed",
            "the train cooldown rc is separate from a qualified queue observation"
        );
        let mut failed = firing("new", "2026-10-16T10:01:00Z", None);
        failed.rc = Some(1);
        assert_eq!(
            project(
                &rules,
                &[("old".into(), old.clone()), ("new".into(), failed)],
                true,
                now
            )["state"],
            "unknown"
        );
        assert_eq!(
            project(&rules, &[("old".into(), old.clone())], false, now)["state"],
            "unknown"
        );
        assert_eq!(
            project(
                &rules,
                &[("old".into(), old)],
                true,
                Some(at("2026-10-16T10:06:00Z"))
            )["state"],
            "unknown"
        );
    }
}
