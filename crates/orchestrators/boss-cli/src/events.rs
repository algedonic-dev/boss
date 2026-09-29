//! `boss events redeliver` — the way out of a dead-lettered outbox row
//! (backlog e22b692e).
//!
//! WHY. Since e4019cbc the outbox relay sets aside a row the bus refuses
//! deterministically (over NATS max_payload, an invalid subject) and
//! delivers the rows behind it; the dispatcher files a backlog-item for
//! each one. Nothing could then act on the row: once the cause was fixed
//! the event stayed off the bus for good, and once a person judged it
//! audit-only it still read as an open dead letter in every count. This
//! verb is the act, with two arms:
//!
//! - `boss events redeliver <outbox id>` puts the row back on the
//!   relay's queue, refused over the staging bound (the bus would only
//!   refuse it again);
//! - `boss events redeliver <outbox id> --resolve --reason-file <PATH>`
//!   closes it without redelivering: the fact stays in audit_log and
//!   off the bus by decision, with the reason on the row. A resolution
//!   is final.
//!
//! A THIN CLIENT OF THE DOOR (adversarial review H1). The first cut
//! connected to Postgres itself and credited a self-asserted actor past
//! any policy check — the shape retired on 2026-09-27 (42da8bd2,
//! a516f1f1). The act now happens on boss-events-api, which owns
//! `event_outbox`: `POST /api/events/outbox/{id}/redeliver|resolve`
//! (`boss_events::outbox_http`), Operator tier, credited to the signed
//! caller. This verb resolves that door the way every SoR verb resolves
//! its service — `BOSS_EVENTS_URL` when it names one, else the
//! `BOSS_JOBS_URL` host on the events port boss-ports assigns (the rule
//! `infra/forge/probe-bin/sor-routes.sh` applies to `/api/events/*`) —
//! signs as the actor running it (refused unnamed, before the socket),
//! and prints the door's answer: the row and the staged fact READ BACK
//! from the database, never the refused payload.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::steps::Wire;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// The event outbox's dead letters: put one back on the relay's queue, or resolve it with a reason.
    Events {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Redeliver a dead-lettered outbox row once the cause of its refusal is fixed, or `--resolve` it with a reason.
    ///
    /// Redelivery clears the row's dead letter so the relay publishes it
    /// on its next drain (its audit_log row already exists, so it is
    /// re-published, not re-logged), and records
    /// `events.outbox.redelivered`. REDELIVERY IS OUT-OF-ORDER DELIVERY:
    /// every later event the relay delivered meanwhile has already
    /// reached subscribers, and the answer says how many. It is refused
    /// over the staging bound, where the bus would refuse it again.
    ///
    /// `--resolve` keeps the row off the bus by decision: it is stamped
    /// resolved with the reason, stops counting as an open dead letter,
    /// and `events.outbox.resolved` is recorded. A resolution is FINAL:
    /// a resolved row is refused by both arms from then on.
    ///
    /// Either arm refuses an id that is not an open dead letter (no such
    /// row, pending, delivered, or already resolved), changing nothing.
    /// The act is done by boss-events-api at the Operator tier and
    /// credited to `BOSS_ACTOR`. The payload is never printed.
    Redeliver {
        /// The dead-lettered row: `event_outbox.id`, as the dead-letter backlog-item's `outbox_id` names it.
        outbox_id: i64,
        /// Resolve the row with a reason instead of redelivering it.
        #[arg(long)]
        resolve: bool,
        /// Why the fact stays off the bus. Requires --resolve.
        #[arg(long, requires = "resolve", conflicts_with = "reason_file")]
        reason: Option<String>,
        /// The reason, read from this file. Requires --resolve.
        #[arg(long, requires = "resolve")]
        reason_file: Option<PathBuf>,
    },
}

/// Names the events door outright (a launcher, a test).
pub(crate) const EVENTS_ENV: &str = "BOSS_EVENTS_URL";

/// What the operator asked for, validated.
#[derive(Debug, PartialEq)]
pub(crate) enum Act {
    Redeliver,
    Resolve(String),
}

/// The act from the flags. clap holds `--reason*` to `--resolve`; what
/// it cannot say is that `--resolve` needs one of the two, and that the
/// reason must carry words — both through `prose`, which names the
/// quoting rule when a substitution ate the text.
pub(crate) fn act_from(
    resolve: bool,
    reason: Option<String>,
    reason_file: Option<&Path>,
) -> Result<Act> {
    if !resolve {
        return Ok(Act::Redeliver);
    }
    crate::prose::text_or_file("--reason", "--reason-file", reason, reason_file).map(Act::Resolve)
}

/// The events door: `events_env` when it names one, else the jobs base
/// re-pointed at the events port — the host is never this verb's own.
pub(crate) fn events_base_from(events_env: Option<String>, jobs_base: &str) -> String {
    events_env
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| crate::owner::on_port(jobs_base, boss_ports::prod("events")))
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Events {
        action:
            Action::Redeliver {
                outbox_id,
                resolve,
                reason,
                reason_file,
            },
    } = cmd;
    let act = act_from(resolve, reason, reason_file.as_deref())?;
    let jobs = crate::gate::resolve_jobs_base(None)?;
    let base = events_base_from(std::env::var(EVENTS_ENV).ok(), &jobs);
    let wire = Wire::at(base, crate::identity::caller())?;
    println!("{}", perform(&wire, outbox_id, &act).await?);
    Ok(())
}

/// One signed POST to the door, and its answer rendered.
pub(crate) async fn perform(wire: &Wire, outbox_id: i64, act: &Act) -> Result<String> {
    let (path, body) = match act {
        Act::Redeliver => (
            format!("/api/events/outbox/{outbox_id}/redeliver"),
            json!({}),
        ),
        Act::Resolve(reason) => (
            format!("/api/events/outbox/{outbox_id}/resolve"),
            json!({ "reason": reason }),
        ),
    };
    let answer = wire
        .call(reqwest::Method::POST, &path, Some(body))
        .await?
        .with_context(|| format!("POST {path} answered with no body"))?;
    render(&answer)
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("?")
}

/// The receipt, from the door's answer: the row's identity and refusal,
/// what the row IS now and the fact that records the act — both read
/// back by the door — and never the payload (the answer carries none).
pub(crate) fn render(answer: &Value) -> Result<String> {
    let letter = answer
        .get("letter")
        .context("the door's answer carries no letter")?;
    let row = answer
        .get("row")
        .context("the door's answer carries no read-back")?;
    let act_kind = text(row, "act_kind");
    let done = if act_kind.ends_with(".resolved") {
        "resolved — stays off the bus by decision (final)".to_string()
    } else {
        let n = letter.get("overtaken_by").and_then(Value::as_i64);
        format!(
            "redelivered — back on the relay's queue; this event reaches subscribers after {} \
             later events (out-of-order delivery)",
            n.map_or("an unknown number of".to_string(), |n| n.to_string())
        )
    };
    let fact = match row.get("act_outbox_id").and_then(Value::as_i64) {
        Some(id) => format!("{act_kind} staged as outbox row {id}"),
        None => format!("NO {act_kind} row found naming it — the act's record is missing"),
    };
    let mut out = format!(
        "outbox row {id} {done}\n  \
         event:      {event_id} ({kind}, staged by {source}, {bytes} bytes)\n  \
         refused:    {at} — {reason}\n  \
         row now:    {state}\n  \
         recorded:   {fact}",
        id = letter.get("outbox_id").and_then(Value::as_i64).unwrap_or(0),
        event_id = text(letter, "event_id"),
        kind = text(letter, "event_kind"),
        source = text(letter, "event_source"),
        bytes = letter
            .get("payload_bytes")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        at = text(letter, "dead_lettered_at"),
        reason = text(letter, "dead_letter_reason"),
        state = text(row, "state"),
    );
    if let Some(resolution) = row.get("resolution").and_then(Value::as_str) {
        out.push_str(&format!("\n  resolution: {resolution}"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::event::Event;
    use clap::Parser;

    #[derive(clap::Parser)]
    struct T {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
        let mut argv = vec!["boss"];
        argv.extend_from_slice(args);
        T::try_parse_from(argv).map(|t| t.cmd)
    }

    #[test]
    fn the_verb_parses_takes_no_database_and_a_reason_needs_resolve() {
        let Cmd::Events {
            action: Action::Redeliver {
                outbox_id, resolve, ..
            },
        } = parse(&["events", "redeliver", "42"]).expect("redeliver parses");
        assert_eq!((outbox_id, resolve), (42, false));
        assert!(
            parse(&[
                "events",
                "redeliver",
                "42",
                "--postgres-url",
                "postgres://x"
            ])
            .is_err(),
            "the verb speaks to the door, never to a database (review H1)"
        );
        for flag in ["--reason", "--reason-file"] {
            assert!(
                parse(&["events", "redeliver", "42", flag, "why"]).is_err(),
                "{flag} without --resolve is refused"
            );
        }
        assert!(
            parse(&[
                "events",
                "redeliver",
                "42",
                "--resolve",
                "--reason",
                "a",
                "--reason-file",
                "b",
            ])
            .is_err(),
            "both reason flags are refused"
        );
    }

    #[test]
    fn a_resolve_needs_a_reason_with_words() {
        assert_eq!(act_from(false, None, None).unwrap(), Act::Redeliver);
        assert!(act_from(true, None, None).is_err(), "no reason");
        assert!(act_from(true, Some("  ".into()), None).is_err(), "blank");
        assert_eq!(
            act_from(true, Some("audit-only by decision".into()), None).unwrap(),
            Act::Resolve("audit-only by decision".into())
        );
    }

    /// The events door keeps the jobs host and moves the port — the
    /// rule sor-routes.sh applies to `/api/events/*`.
    #[test]
    fn the_events_door_is_the_jobs_host_on_the_events_port() {
        let port = boss_ports::prod("events");
        assert_eq!(
            events_base_from(None, "http://192.0.2.34:7900"),
            format!("http://192.0.2.34:{port}")
        );
        assert_eq!(
            events_base_from(Some("http://ev:1/".into()), "http://192.0.2.34:7900"),
            "http://ev:1"
        );
        assert_eq!(
            events_base_from(Some("  ".into()), "http://192.0.2.34:7900"),
            format!("http://192.0.2.34:{port}")
        );
    }

    const SECRET: &str = "PAYLOAD-THAT-MUST-NOT-PRINT";

    async fn dead_letter(db: &boss_testing::TestDb) -> i64 {
        use chrono::TimeZone;
        // A fixed instant, not the wall clock: the no-wallclock lint
        // reads src/ test modules too.
        let at = chrono::Utc
            .with_ymd_and_hms(2026, 9, 28, 6, 0, 0)
            .single()
            .unwrap();
        let e = Event {
            id: uuid::Uuid::new_v4(),
            timestamp: at,
            source: "cli-test".into(),
            kind: "cli.test.refused".into(),
            payload: serde_json::json!({ "blob": SECRET }),
        };
        let mut tx = db.pool.begin().await.unwrap();
        boss_events::outbox::record_event_in_tx(&mut tx, &e)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query_scalar(
            "UPDATE event_outbox SET dead_lettered_at = $2, \
             dead_letter_reason = 'max payload size exceeded' \
             WHERE event_id = $1 RETURNING id",
        )
        .bind(e.id)
        .bind(at)
        .fetch_one(&db.pool)
        .await
        .unwrap()
    }

    /// Serve the REAL door over a TestDb, the wire production runs.
    async fn serve(db: &boss_testing::TestDb) -> String {
        let app = boss_events::outbox_http::outbox_router(db.pool.clone()).layer(
            axum::middleware::from_fn(boss_policy_client::request_context_middleware),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn named(id: &str) -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: id.into(),
            source: crate::identity::Source::Env,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_arm_goes_through_the_door_signed_and_prints_the_read_back() {
        let db = boss_testing::TestDb::new().await;
        let base = serve(&db).await;
        let wire = Wire::at(base.clone(), named("emp-cli-test")).unwrap();

        let id = dead_letter(&db).await;
        let out = perform(&wire, id, &Act::Redeliver)
            .await
            .expect("redelivered");
        assert!(
            out.contains(&format!("outbox row {id} redelivered")),
            "{out}"
        );
        assert!(
            out.contains("reaches subscribers after 0 later events"),
            "{out}"
        );
        assert!(out.contains("cli.test.refused"), "{out}");
        assert!(out.contains("max payload size exceeded"), "{out}");
        assert!(out.contains("row now:    pending"), "read back: {out}");
        assert!(
            out.contains("events.outbox.redelivered staged as outbox row"),
            "names the fact: {out}"
        );
        assert!(!out.contains(SECRET), "never the payload: {out}");
        let actor: String = sqlx::query_scalar(
            "SELECT payload->>'_actor' FROM event_outbox WHERE kind = 'events.outbox.redelivered'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(actor, "emp-cli-test", "credited to the signed caller");

        let id = dead_letter(&db).await;
        let out = perform(&wire, id, &Act::Resolve("audit-only by decision".into()))
            .await
            .expect("resolved");
        assert!(out.contains(&format!("outbox row {id} resolved")), "{out}");
        assert!(out.contains("resolution: audit-only by decision"), "{out}");
        assert!(out.contains("row now:    dead-lettered, resolved"), "{out}");
        assert!(
            out.contains("events.outbox.resolved staged as outbox row"),
            "{out}"
        );
        assert!(!out.contains(SECRET), "never the payload: {out}");

        // A refusal carries the door's words, and nothing changes.
        let err = perform(&wire, id, &Act::Redeliver)
            .await
            .expect_err("resolved is final");
        assert!(err.to_string().contains("final"), "{err}");
        assert!(err.to_string().contains("409"), "{err}");

        // Unnamed, the write never reaches the socket.
        let unnamed = Wire::at(base, None).unwrap();
        let id = dead_letter(&db).await;
        let err = perform(&unnamed, id, &Act::Redeliver)
            .await
            .expect_err("unnamed write refused");
        assert!(err.to_string().contains("BOSS_ACTOR"), "{err}");
        assert_eq!(
            boss_events::outbox::dead_lettered_count(&db.pool)
                .await
                .unwrap(),
            1,
            "the unnamed attempt changed nothing"
        );
    }
}
