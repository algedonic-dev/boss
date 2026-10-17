//! Delivery time over the existing train/channel record, with its denominator.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) async fn run() -> anyhow::Result<()> {
    let door = crate::steps::Wire::at(crate::sor_client::jobs_base()?, crate::identity::caller())?;
    let report = observe(&door, boss_clock_client::wall_now()).await;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report["status"] != "complete" {
        anyhow::bail!(
            "delivery timing population is unread; original native pages are retained in the JSON report"
        );
    }
    Ok(())
}

async fn observe(door: &dyn crate::car_retire::Door, now: DateTime<Utc>) -> Value {
    let mut reads = Vec::new();
    let trains = match crate::delivery_plan::read_all(
        door,
        "/api/jobs?kind=pr-train&full=true",
        &mut reads,
    )
    .await
    {
        Ok(trains) => trains,
        Err(error) => {
            return json!({"schema":1,"status":"unread","error":format!("{error:#}"),"native_reads":reads});
        }
    };
    if trains.iter().any(|train| train["kind"] != "pr-train") {
        return json!({"schema":1,"status":"unread","error":"native population contains another packet kind","native_reads":reads});
    }
    let mut report = reading(&trains, now);
    report["status"] = json!("complete");
    report["native_reads"] = json!(reads);
    report
}

/// A projection of existing completion stamps. Whole-second durations use the
/// same arrival_report as the conductor; the original clock strings stay beside
/// each observation. Channel labels and recorded completion evidence are separate.
pub(crate) fn reading(trains: &[Value], now: DateTime<Utc>) -> Value {
    let mut counts = BTreeMap::from([
        ("sample", 0usize),
        ("right_censored", 0),
        ("missing", 0),
        ("invalid", 0),
        ("closed_without_arrival_stamp", 0),
    ]);
    let mut groups: BTreeMap<(String, String), Vec<i64>> = BTreeMap::new();
    let mut observations = Vec::new();
    let mut unknown_channel = 0;
    let mut unknown_basis = 0;
    for train in trains {
        let recorded_channel = train
            .pointer("/metadata/delivery_channel")
            .and_then(Value::as_str);
        let channel = recorded_channel
            .and_then(crate::channels::DeliveryChannel::parse)
            .map_or("unknown", |c| c.label());
        unknown_channel += usize::from(channel == "unknown");
        let converged = boss_jobs::car::find_step(train, "converged", "Cluster converged");
        let evidence = converged.and_then(|s| s.pointer("/metadata/delivery_evidence"));
        let basis = match evidence {
            Some(e) if e["arrived"] == true && e["basis"] == "data" => "data",
            Some(e) if e["arrived"] == true && e["basis"] == "image" => "image",
            _ if converged
                .and_then(|s| s.pointer("/metadata/cluster_commit"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()) =>
            {
                "image"
            }
            _ => "unknown",
        };
        unknown_basis += usize::from(basis == "unknown");
        let report = crate::train::arrival_report(train, &[]);
        let timings = &report["timings"];
        let start = timings["boarded_at"]
            .as_str()
            .map(DateTime::parse_from_rfc3339);
        let end = timings["arrived_at"]
            .as_str()
            .map(DateTime::parse_from_rfc3339);
        let mut duration = None;
        let mut censored = None;
        let disposition = match (&start, &end) {
            (Some(Err(_)), _) | (_, Some(Err(_))) => "invalid",
            (Some(Ok(a)), Some(Ok(b))) if a > b || *b > now.fixed_offset() => "invalid",
            (_, Some(Ok(_)))
                if train["status"] != "closed"
                    || !boss_jobs::car::find_step(train, "arrived", "Train arrived")
                        .is_some_and(|s| s["status"] == "completed") =>
            {
                "invalid"
            }
            (Some(Ok(_)), Some(Ok(_))) => {
                duration = timings["total_s"].as_i64();
                if duration.is_some() {
                    "sample"
                } else {
                    "missing"
                }
            }
            (_, Some(Ok(_))) => "missing",
            (Some(Ok(a)), None) if *a > now.fixed_offset() => "invalid",
            (_, None) if train["status"] == "closed" => "closed_without_arrival_stamp",
            (Some(Ok(a)), None) if train["status"] == "open" => {
                censored = Some((now.fixed_offset() - *a).num_seconds());
                "right_censored"
            }
            _ => "missing",
        };
        *counts.entry(disposition).or_default() += 1;
        if let Some(value) = duration {
            groups
                .entry((channel.to_owned(), basis.to_owned()))
                .or_default()
                .push(value);
        }
        observations.push(json!({"train":train["id"],"channel":channel,
            "recorded_channel":recorded_channel,"completion_basis":basis,"timings":timings,
            "disposition":disposition,"duration_s":duration,"censored_elapsed_s":censored}));
    }
    let groups: Vec<_> = groups
        .into_iter()
        .map(|((channel, basis), mut values)| {
            values.sort_unstable();
            let n = values.len();
            let median = if n % 2 == 0 {
                (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
            } else {
                values[n / 2] as f64
            };
            json!({"channel":channel,"completion_basis":basis,"samples":n,
            "min_s":values.first(),"median_s":median,"max_s":values.last(),"durations_s":values})
        })
        .collect();
    json!({"schema":1,"population":trains.len(),"counts":counts,
        "unknown_channel":unknown_channel,"unknown_completion_basis":unknown_basis,
        "measure":"boarded to actual arrived; whole seconds; original stamps retained",
        "as_of":now.to_rfc3339(),"groups":groups,"observations":observations})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    struct Rows(Value);
    #[async_trait::async_trait]
    impl crate::car_retire::Door for Rows {
        async fn send(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
        ) -> anyhow::Result<Option<Value>> {
            assert_eq!(method, reqwest::Method::GET);
            assert!(
                path.contains("&full=true&"),
                "timing requires native step metadata: {path}"
            );
            assert!(body.is_none());
            Ok(Some(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn a_partial_native_population_is_unread_and_retains_the_original_page() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-16T01:20:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let partial = json!({"total":1,"data":[],"native_detail":"full original page"});
        let unread = observe(&Rows(partial.clone()), now).await;
        assert_eq!(unread["status"], "unread");
        assert_eq!(unread["native_reads"][0]["answer"], partial);
        let mut row = train(
            "one",
            "closed",
            "data",
            Some("2026-10-16T01:00:00Z"),
            Some("2026-10-16T01:10:00Z"),
            "data",
        );
        row["kind"] = json!("pr-train");
        let complete = observe(&Rows(json!({"total":1,"data":[row]})), now).await;
        assert_eq!(complete["status"], "complete");
        assert_eq!(complete["population"], 1);
        assert_eq!(complete["counts"]["sample"], 1);
    }

    fn train(
        id: &str,
        status: &str,
        channel: &str,
        start: Option<&str>,
        end: Option<&str>,
        basis: &str,
    ) -> Value {
        json!({"id":id,"status":status,"metadata":{"delivery_channel":channel},"steps":[
            {"spec_slug":"collect","metadata":{"completed_at":start}},
            {"spec_slug":"converged","metadata":if basis == "data" {
                json!({"delivery_evidence":{"basis":"data","arrived":true}})
            } else { json!({"cluster_commit":"abcdef0123456789"}) }},
            {"spec_slug":"arrived","status":if end.is_some(){"completed"}else{"pending"},"metadata":{"completed_at":end}}
        ]})
    }

    #[test]
    fn timing_keeps_old_data_stamps_separate_from_image_completion_and_conserves_every_row() {
        let start = "2026-10-16T01:00:00Z";
        let end = "2026-10-16T01:10:00Z";
        let rows = vec![
            train(
                "legacy-data",
                "closed",
                "data",
                Some(start),
                Some(end),
                "image",
            ),
            train(
                "native-data",
                "closed",
                "data",
                Some(start),
                Some(end),
                "data",
            ),
            train("open", "open", "software", Some(start), None, "image"),
            train("missing", "closed", "software", None, Some(end), "image"),
            train(
                "reversed",
                "closed",
                "software",
                Some(end),
                Some(start),
                "image",
            ),
            train(
                "unknown-channel",
                "closed",
                "unread",
                Some(start),
                Some(end),
                "image",
            ),
            train("no-arrival", "closed", "config", Some(start), None, "image"),
        ];
        let original = rows.clone();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-16T01:20:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let report = reading(&rows, now);
        assert_eq!(report["population"], 7);
        assert_eq!(
            report["counts"],
            json!({"sample":3,"right_censored":1,"missing":1,"invalid":1,"closed_without_arrival_stamp":1})
        );
        assert_eq!(report["unknown_channel"], 1);
        assert_eq!(report["observations"].as_array().unwrap().len(), rows.len());
        let groups = report["groups"].as_array().unwrap();
        assert!(groups.iter().any(|g| g["channel"] == "data"
            && g["completion_basis"] == "image"
            && g["durations_s"] == json!([600])));
        assert!(groups.iter().any(|g| g["channel"] == "data"
            && g["completion_basis"] == "data"
            && g["durations_s"] == json!([600])));
        assert_eq!(report["observations"][2]["censored_elapsed_s"], 1200);
        assert_eq!(rows, original);
    }

    #[test]
    fn malformed_future_and_unknown_clocks_never_become_zero_duration_samples() {
        let mut rows = vec![
            train(
                "bad",
                "closed",
                "software",
                Some("unread"),
                Some("2026-10-16T01:10:00Z"),
                "image",
            ),
            train(
                "future",
                "open",
                "software",
                Some("2026-10-17T01:00:00Z"),
                None,
                "image",
            ),
        ];
        rows.push(train(
            "negative-fraction",
            "closed",
            "software",
            Some("2026-10-16T01:00:00.000000002Z"),
            Some("2026-10-16T01:00:00.000000001Z"),
            "image",
        ));
        rows.push(json!({"id":"legacy","status":"closed","steps":[]}));
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-16T01:20:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let report = reading(&rows, now);
        assert_eq!(report["counts"]["sample"], 0);
        assert_eq!(report["counts"]["invalid"], 3);
        assert_eq!(report["counts"]["closed_without_arrival_stamp"], 1);
        assert_eq!(report["unknown_completion_basis"], 1);
    }
}
