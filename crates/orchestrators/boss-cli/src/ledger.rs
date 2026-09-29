//! `boss ledger` — list the ledger's periods, lock or unlock a month,
//! through boss-ledger-api's signed doors (backlog 05cd6572, 52dc6ffb).
//!
//! WHY. Until this car every `boss ledger` verb opened Postgres itself,
//! with `--postgres-url` defaulting to the demo credentials on 127.0.0.1.
//! `lock` and `unlock` stamped their event `automation:operator-cli`,
//! `lock` wrote `locked_by` from a free-text `--locked-by` (default
//! `operator`), and `unlock` recorded `operator-cli` as its actor. So a
//! person's act on the books was credited to an automation (the
//! forged-actor failure of 2026-08-27, backlog 5083d6f5), passed no
//! policy check at all, and landed in whatever database answered on the
//! local port, which need not be the system of record (a516f1f1,
//! 42da8bd2 measured both). The doors that do it properly already
//! existed: `POST /api/ledger/periods/{id}/lock|unlock`
//! (`boss_ledger::http::periods`) author the lock from the SIGNED caller
//! and refuse a body naming anyone else (backlog 975c228f), behind the
//! ledger's read gate and its auditor refusal. This verb is now a thin
//! client of them, the shape `boss tenant stamp` took for 42da8bd2.
//!
//! WHICH SERVICE. boss-ledger-api, on its own `boss_ports` port of the
//! machine door: `BOSS_JOBS_URL`'s host, the ledger port — the rule
//! `Bases::on_door` applies to every registry and `boss attach` to the
//! file store, from the one function all three call
//! ([`crate::tenant_publish::service_on_door`]). A base with no host is
//! refused rather than resolved into a URL that would read another stack.
//!
//! WHO IT SIGNS AS. `BOSS_ACTOR`, like every write verb: the period's
//! `locked_by` and the `ledger.period.locked|unlocked` event name the
//! actor running the command, and an unnamed write is refused before its
//! socket. There is no `--locked-by`: the door would refuse any name but
//! the signer's, so the flag could only ever say the same thing or fail.
//!
//! WHAT WENT ELSEWHERE. `boss ledger rebuild` repeated, line for line,
//! the `ledger-journal` step of `boss-rebuild-all` (the same
//! `boss_ledger::rebuild`), and both scripts that called it ran it right
//! after a full `boss-rebuild-all` had already done so. It is deleted:
//! `boss-rebuild-all --database-url <URL> --only ledger-journal` is the
//! one rebuild, with its database spelled out. `boss assets
//! rebuild-projection` moved there too, as the `assets` step.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::steps::Wire;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Ledger periods — list them, lock or unlock a month — through boss-ledger-api's signed doors.
    ///
    /// Rebuilding the GL projection is `boss-rebuild-all --database-url
    /// <URL> --only ledger-journal`, not a `boss` verb.
    Ledger {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// List all periods with their status and totals, as boss-ledger-api answers them.
    Periods {
        /// The door's answer as JSON, unchanged.
        #[arg(long)]
        json: bool,
    },
    /// Lock a monthly period by its starting date (YYYY-MM-DD).
    ///
    /// Pins the active posting-rule version and writes a checksum;
    /// further writes to the period are refused until it is unlocked.
    /// The lock is credited to `BOSS_ACTOR` — the signed caller, never a
    /// name the command is given.
    Lock {
        /// The month's starts_on date, e.g. 2026-03-01.
        starts_on: String,
    },
    /// Unlock a monthly period by its starting date (YYYY-MM-DD).
    ///
    /// Clears the lock and returns the period to `open`, credited to
    /// `BOSS_ACTOR`.
    Unlock {
        /// The month's starts_on date, e.g. 2026-03-01.
        starts_on: String,
    },
}

/// The ledger door for a jobs base: the same host, the ledger's port.
pub(crate) fn ledger_base(jobs_base: &str) -> Result<String> {
    crate::tenant_publish::service_on_door(jobs_base, "ledger")
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Ledger { action } = cmd;
    let jobs = crate::gate::resolve_jobs_base(None)?;
    let wire = Wire::at(ledger_base(&jobs)?, crate::identity::caller())?;
    let out = match action {
        Action::Periods { json } => periods(&wire, json).await?,
        Action::Lock { starts_on } => lock(&wire, &starts_on).await?,
        Action::Unlock { starts_on } => unlock(&wire, &starts_on).await?,
    };
    println!("{out}");
    Ok(())
}

/// One period as `GET /api/ledger/periods` answers it — only the fields
/// this verb prints or matches on.
#[derive(Debug, Deserialize)]
struct PeriodRow {
    id: String,
    kind: String,
    starts_on: String,
    status: String,
    #[serde(default)]
    locked_by: Option<String>,
    #[serde(default)]
    entry_count: i64,
    #[serde(default)]
    total_debits: i64,
    #[serde(default)]
    total_credits: i64,
}

async fn read_periods(wire: &Wire) -> Result<(Value, Vec<PeriodRow>)> {
    let answer = wire
        .call(reqwest::Method::GET, "/api/ledger/periods", None)
        .await?
        .context("GET /api/ledger/periods answered with no body")?;
    let rows = serde_json::from_value(answer.clone())
        .context("GET /api/ledger/periods answered something that is not a list of periods")?;
    Ok((answer, rows))
}

/// The monthly period starting on `starts_on`, as the door lists it now.
/// A date that does not parse is refused before any socket; a month the
/// ledger does not hold is refused naming the date.
async fn month(wire: &Wire, starts_on: &str) -> Result<PeriodRow> {
    let date: chrono::NaiveDate = starts_on
        .parse()
        .map_err(|e| anyhow::anyhow!("bad date `{starts_on}` (want YYYY-MM-DD): {e}"))?;
    let want = date.to_string();
    let (_, rows) = read_periods(wire).await?;
    rows.into_iter()
        .find(|p| p.kind == "month" && p.starts_on == want)
        .with_context(|| format!("the ledger holds no monthly period starting {want}"))
}

pub(crate) async fn periods(wire: &Wire, json: bool) -> Result<String> {
    let (answer, rows) = read_periods(wire).await?;
    if json {
        return Ok(serde_json::to_string_pretty(&answer)?);
    }
    let mut out = format!(
        "{:<12}  {:<6}  {:<8}  {:>8}  {:>14}  {:>14}  LOCKED_BY\n{}",
        "STARTS_ON",
        "KIND",
        "STATUS",
        "ENTRIES",
        "DEBITS",
        "CREDITS",
        "-".repeat(88)
    );
    for p in &rows {
        out.push_str(&format!(
            "\n{:<12}  {:<6}  {:<8}  {:>8}  {:>14}  {:>14}  {}",
            p.starts_on,
            p.kind,
            p.status,
            p.entry_count,
            p.total_debits,
            p.total_credits,
            p.locked_by.as_deref().unwrap_or("-")
        ));
    }
    Ok(out)
}

/// Lock the month through the door, then read the period back: the line
/// printed is what the ledger now holds, not what was asked for.
pub(crate) async fn lock(wire: &Wire, starts_on: &str) -> Result<String> {
    let period = month(wire, starts_on).await?;
    let path = format!("/api/ledger/periods/{}/lock", period.id);
    let answer = wire
        .call(reqwest::Method::POST, &path, Some(json!({})))
        .await?
        .with_context(|| format!("POST {path} answered with no body"))?;
    let checksum = answer
        .get("checksum")
        .and_then(Value::as_str)
        .with_context(|| format!("POST {path} answered with no checksum: {answer}"))?
        .to_string();
    let now = month(wire, starts_on).await?;
    if now.status != "locked" {
        bail!(
            "POST {path} answered locked, but the period reads back `{}`",
            now.status
        );
    }
    Ok(format!(
        "locked period {} — {checksum}, locked by {}",
        now.starts_on,
        now.locked_by.as_deref().unwrap_or("?")
    ))
}

/// Unlock the month through the door, then read the period back.
pub(crate) async fn unlock(wire: &Wire, starts_on: &str) -> Result<String> {
    let period = month(wire, starts_on).await?;
    let path = format!("/api/ledger/periods/{}/unlock", period.id);
    wire.call(reqwest::Method::POST, &path, Some(json!({})))
        .await?
        .with_context(|| format!("POST {path} answered with no body"))?;
    let now = month(wire, starts_on).await?;
    if now.status != "open" {
        bail!(
            "POST {path} answered open, but the period reads back `{}`",
            now.status
        );
    }
    Ok(format!("unlocked period {} — status open", now.starts_on))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Caller, Source};
    use boss_testing::TestDb;
    use std::sync::Arc;

    /// The REAL ledger router over a TestDb, on a loopback socket, with
    /// the request-context layer that turns `x-boss-user` into the
    /// caller — the handler, author rule and SQL production runs.
    async fn serve(db: &TestDb) -> String {
        let app = boss_ledger::http::router(boss_ledger::http::LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            // The read gate is not this test's subject.
            policy: Arc::new(boss_policy_client::PermissivePolicyClient),
        })
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    async fn a_month(db: &TestDb, starts_on: &str, ends_on: &str) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
             VALUES ($1, 'month', $2::date, $3::date, 'open')",
        )
        .bind(id)
        .bind(starts_on)
        .bind(ends_on)
        .execute(&db.pool)
        .await
        .unwrap();
        id
    }

    async fn row(db: &TestDb, id: uuid::Uuid) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, locked_by FROM gl_periods WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap()
    }

    /// The actor of the newest staged event of `kind` — the outbox row
    /// the lock's own transaction recorded.
    async fn staged_actor(db: &TestDb, kind: &str) -> String {
        let (payload,): (Value,) = sqlx::query_as(
            "SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(kind)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        payload["_actor"].as_str().unwrap_or_default().to_string()
    }

    fn named(id: &str) -> Option<Caller> {
        Some(Caller {
            id: id.into(),
            source: Source::Env,
        })
    }

    /// The door is the jobs base's host on the ledger's port; the host
    /// is never the verb's own, and a base with none is refused.
    #[test]
    fn the_ledger_door_is_the_jobs_host_on_the_ledger_port() {
        assert_eq!(
            ledger_base("http://door.test:7900").unwrap(),
            format!("http://door.test:{}", boss_ports::prod("ledger"))
        );
        let err = ledger_base("http://:7900").unwrap_err();
        assert!(err.to_string().contains("names no host"), "{err:#}");
    }

    /// Lock and unlock go through the door, signed as the actor running
    /// the verb: the row's `locked_by` and the staged event name that
    /// actor — never `operator`, never `automation:operator-cli`.
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_and_unlock_are_credited_to_the_signed_actor() {
        let db = TestDb::new().await;
        let base = serve(&db).await;
        let id = a_month(&db, "2098-03-01", "2098-03-31").await;
        let wire = Wire::at(base, named("claude@algedonic.dev")).unwrap();

        let line = lock(&wire, "2098-03-01").await.unwrap();
        let (status, locked_by) = row(&db, id).await;
        assert_eq!(status, "locked");
        let locked_by = locked_by.expect("a locked period names its locker");
        assert!(
            locked_by.contains("claude@algedonic.dev"),
            "locked_by is the signer: {locked_by}"
        );
        assert!(line.starts_with("locked period 2098-03-01 — "), "{line}");
        assert!(line.ends_with(&format!("locked by {locked_by}")), "{line}");
        let actor = staged_actor(&db, "ledger.period.locked").await;
        assert!(actor.contains("claude@algedonic.dev"), "{actor}");
        assert!(!actor.contains("operator-cli"), "{actor}");

        let line = unlock(&wire, "2098-03-01").await.unwrap();
        assert_eq!(line, "unlocked period 2098-03-01 — status open");
        assert_eq!(row(&db, id).await.0, "open");
        let actor = staged_actor(&db, "ledger.period.unlocked").await;
        assert!(actor.contains("claude@algedonic.dev"), "{actor}");
    }

    /// An unnamed write is refused before its socket, naming the fix,
    /// and the period is untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unnamed_lock_is_refused_and_writes_nothing() {
        let db = TestDb::new().await;
        let base = serve(&db).await;
        let id = a_month(&db, "2098-04-01", "2098-04-30").await;
        let err = lock(&Wire::at(base, None).unwrap(), "2098-04-01")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("BOSS_ACTOR"), "{err:#}");
        assert_eq!(row(&db, id).await, ("open".to_string(), None));
    }

    /// A month the ledger does not hold is refused naming the date, and
    /// a year starting the same day is not mistaken for it; a date that
    /// does not parse is refused before any read.
    #[tokio::test(flavor = "multi_thread")]
    async fn only_a_month_the_ledger_holds_is_locked() {
        let db = TestDb::new().await;
        let base = serve(&db).await;
        sqlx::query(
            "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
             VALUES ($1, 'year', '2097-01-01', '2097-12-31', 'open')",
        )
        .bind(uuid::Uuid::new_v4())
        .execute(&db.pool)
        .await
        .unwrap();
        let wire = Wire::at(base, named("claude@algedonic.dev")).unwrap();
        let err = lock(&wire, "2097-01-01").await.unwrap_err();
        assert!(
            err.to_string()
                .contains("no monthly period starting 2097-01-01"),
            "{err:#}"
        );
        let err = lock(&wire, "March").await.unwrap_err();
        assert!(err.to_string().contains("bad date `March`"), "{err:#}");
    }

    /// `periods` renders what the door lists, and `--json` is the door's
    /// answer unchanged.
    #[tokio::test(flavor = "multi_thread")]
    async fn periods_is_the_doors_list() {
        let db = TestDb::new().await;
        let base = serve(&db).await;
        a_month(&db, "2098-05-01", "2098-05-31").await;
        let wire = Wire::at(base, named("claude@algedonic.dev")).unwrap();
        let table = periods(&wire, false).await.unwrap();
        assert!(
            table
                .lines()
                .any(|l| l.starts_with("2098-05-01") && l.contains("month") && l.contains("open")),
            "{table}"
        );
        let json: Value = serde_json::from_str(&periods(&wire, true).await.unwrap()).unwrap();
        assert!(
            json.as_array()
                .unwrap()
                .iter()
                .any(|p| p["starts_on"] == "2098-05-01"),
            "{json}"
        );
    }
}
