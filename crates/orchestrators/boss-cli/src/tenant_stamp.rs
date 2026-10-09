//! The tenant publish stamp: a successful `boss tenant publish` records
//! itself in the database it published into, and the launcher
//! publishes only while there is no such record (backlog 6a8d4972,
//! design e187198f "the instance is the truth", car 2).
//!
//! WHY. Measured 2026-09-18: infra/oss-quickstart/services-launcher.sh
//! -> tenant-launch.sh -> infra/seed-tenant.sh ran `boss tenant
//! publish` at EVERY services-container start with no guard, so a
//! converge's ConfigMap rebuild implied a publish. Car 1 made every
//! door insert-if-absent and `--take` the only overwrite; this car
//! makes the LAUNCHER's automatic publish once per database: the
//! seeds bootstrap a fresh instance (the OSS quickstart, the
//! playground, a switched database), and after that a new repo row
//! reaches a running instance through an operator's own `boss tenant
//! publish` — still insert-if-absent, the stamp gates nothing the
//! operator runs by hand — or a boot with BOSS_TENANT_TAKE naming the
//! registries to overwrite.
//!
//! THE STAMP IS A ROW, NOT A BELIEF. One row per successful publish in
//! `tenant_publishes` (infra/postgres/schema/20260918200200-…), append-
//! only: the first row's date is what the launcher prints, a later row
//! is the record of a republish and what it took. The row projects a
//! FACT in the log — one `tenant.published` event staged on the outbox
//! in the same transaction (backlog dbdc4d31) — and the row stays as
//! the launcher's fast read.
//!
//! THE STAMP IS WRITTEN THROUGH THE JOBS API (backlog 42da8bd2). Until
//! 2026-09-27 the verb wrote row and event straight into
//! BOSS_POSTGRES_URL, so a publish from a seat with no database — the
//! operator's `--door` route — printed "not stamped" and left NEITHER:
//! measured that day, no `tenant.*` event had ever reached the live
//! audit log, and the 2026-09-25 publish to prod that filed 42da8bd2 is
//! on no record. Now the verb posts the stamp to `POST
//! /api/tenant/publishes` on the same jobs service its other writes
//! just went through (`boss_jobs::tenant_publishes`), on EVERY route:
//! the transaction lives once, in the service, and the door credits the
//! signed caller and answers with the stamp as recorded, which is what
//! the verb prints. BOSS_POSTGRES_URL is now only the READ the
//! launcher's guard makes (`boss tenant published`).
//!
//! PORTS, the crate's shape: [`PublishStamps`] is the write the verb
//! needs ([`HttpStamps`] in the binary), [`PublishedStamps`] the read
//! the guard needs ([`PgStamps`]); `InMemoryStamps` proves the verb's
//! decisions without either.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub use boss_jobs::tenant_publishes::{NewStamp, Stamp};
#[cfg(test)]
use boss_jobs::tenant_publishes::{TENANT_PUBLISHED, published_event};

/// What the launcher reads: the first publish and how many there are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub first: Stamp,
    pub count: i64,
    pub last_at: DateTime<Utc>,
}

fn rfc3339(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Published {
    /// ONE line, the date first: tenant-launch.sh takes the first word
    /// as the stamp date for its own line, and an operator reads the
    /// rest. Pinned by `renders_the_date_first`.
    pub fn render(&self) -> String {
        format!(
            "{} tenant {} published by {} (boss {}); {} publish{}, last {}",
            rfc3339(&self.first.published_at),
            self.first.tenant_id,
            self.first.published_by,
            self.first.boss_commit,
            self.count,
            if self.count == 1 { "" } else { "es" },
            rfc3339(&self.last_at),
        )
    }
}

/// The write `boss tenant publish` needs: record one publish, signed as
/// `actor`, and hand back the stamp AS RECORDED — the door's receipt,
/// not the verb's belief about it.
#[async_trait]
pub trait PublishStamps: Send + Sync {
    async fn record(&self, stamp: &NewStamp, actor: &str) -> Result<Stamp>;
}

/// The read `boss tenant published` needs: the first publish recorded
/// in this database, with the count, or `None` for a database no
/// publish has stamped.
#[async_trait]
pub trait PublishedStamps: Send + Sync {
    async fn published(&self) -> Result<Option<Published>>;
}

/// The jobs API's stamp door — the base is the SAME jobs base the
/// publish's own writes went to (`Bases::jobs`: the machine door's jobs
/// port, a gateway, or the in-pod localhost port), so the stamp lands
/// in the instance that was published.
pub struct HttpStamps {
    base: String,
    client: boss_core::machine_token::Client,
}

impl HttpStamps {
    /// The stamp door on `bases.jobs`. Through a gateway its client
    /// stamps no machine token, the rule the publish walk itself takes
    /// (`tenant_publish::walk_client`, backlog 2ee29275).
    pub fn new(bases: &crate::tenant_publish::Bases) -> Result<Self> {
        let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
        Ok(Self {
            base: bases.jobs.trim_end_matches('/').to_string(),
            client: if bases.through_gateway {
                boss_core::machine_token::Client::unstamped(builder)?
            } else {
                crate::gate::machine_client_with(builder)?
            },
        })
    }
}

#[async_trait]
impl PublishStamps for HttpStamps {
    async fn record(&self, stamp: &NewStamp, actor: &str) -> Result<Stamp> {
        let url = format!("{}/api/tenant/publishes", self.base);
        let resp = self
            .client
            .post(&url)
            // Signed as the caller the verb runs as — the door credits
            // this header's id as published_by, never a body field.
            .header("x-boss-user", crate::identity::header(actor))
            .json(stamp)
            .send()
            .await
            .with_context(|| format!("POST {url} (the tenant publish stamp)"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("POST {url} answered {status}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("POST {url} answered {status} with no stamp: {text}"))
    }
}

/// In memory, for the verb's tests: both ports, no database. Holds the
/// rows and the events beside them, so a test reads what was recorded
/// on both sides.
#[cfg(test)]
#[derive(Default)]
pub struct InMemoryStamps(std::sync::Mutex<Vec<(Stamp, boss_core::event::Event)>>);

#[cfg(test)]
impl InMemoryStamps {
    pub fn push(&self, stamp: Stamp) {
        let event = published_event(&stamp);
        self.0.lock().unwrap().push((stamp, event));
    }

    pub fn events(&self) -> Vec<boss_core::event::Event> {
        self.0
            .lock()
            .map(|rows| rows.iter().map(|(_, e)| e.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[async_trait]
impl PublishStamps for InMemoryStamps {
    async fn record(&self, stamp: &NewStamp, actor: &str) -> Result<Stamp> {
        let st = stamp
            .credited(actor, boss_clock_client::wall_now())
            .map_err(anyhow::Error::msg)?;
        self.push(st.clone());
        Ok(st)
    }
}

#[cfg(test)]
#[async_trait]
impl PublishedStamps for InMemoryStamps {
    async fn published(&self) -> Result<Option<Published>> {
        let rows = self.0.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(summarise(rows.iter().map(|(s, _)| s.clone())))
    }
}

/// PURE: the first-by-date stamp, the count and the latest date, from
/// rows in any order — the same answer the SQL below gives.
#[cfg(test)]
fn summarise(rows: impl Iterator<Item = Stamp>) -> Option<Published> {
    let mut rows: Vec<Stamp> = rows.collect();
    rows.sort_by_key(|s| s.published_at);
    let first = rows.first()?.clone();
    let last_at = rows.last()?.published_at;
    Some(Published {
        first,
        count: rows.len() as i64,
        last_at,
    })
}

/// The database, READ: `BOSS_POSTGRES_URL` is the services container's
/// spelling (docker-compose.yml, the cluster manifests); the same one
/// the sim tenant's baseline stamp and every service read.
pub struct PgStamps(sqlx::PgPool);

impl PgStamps {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .context("connecting to BOSS_POSTGRES_URL for the tenant publish stamp")?;
        Ok(Self(pool))
    }

    #[cfg(test)]
    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self(pool)
    }
}

#[async_trait]
impl PublishedStamps for PgStamps {
    async fn published(&self) -> Result<Option<Published>> {
        let first: Option<(String, DateTime<Utc>, String, String, Vec<String>, i32)> =
            sqlx::query_as(
                "SELECT tenant_id, published_at, published_by, boss_commit, took, writes \
                 FROM tenant_publishes ORDER BY published_at, id LIMIT 1",
            )
            .fetch_optional(&self.0)
            .await
            .context("reading tenant_publishes")?;
        let Some((tenant_id, published_at, published_by, boss_commit, took, writes)) = first else {
            return Ok(None);
        };
        let (count, last_at): (i64, DateTime<Utc>) =
            sqlx::query_as("SELECT COUNT(*), MAX(published_at) FROM tenant_publishes")
                .fetch_one(&self.0)
                .await
                .context("counting tenant_publishes")?;
        Ok(Some(Published {
            first: Stamp {
                tenant_id,
                published_at,
                published_by,
                boss_commit,
                took,
                writes,
            },
            count,
            last_at,
        }))
    }
}

/// The one line `boss tenant publish` prints about its stamp, rendered
/// from the stamp the door RECORDED. A stamp that did not land after a
/// publish that did IS an error — the launcher's contract is "published
/// and stamped", and its retry republishes idempotently until both hold.
pub async fn stamp_after_publish(
    stamps: &dyn PublishStamps,
    stamp: &NewStamp,
    actor: &str,
) -> Result<String> {
    let recorded = stamps.record(stamp, actor).await?;
    Ok(format!(
        "stamped: tenant {} publish recorded in tenant_publishes at {} by {} (boss {}){}",
        recorded.tenant_id,
        rfc3339(&recorded.published_at),
        recorded.published_by,
        recorded.boss_commit,
        if recorded.took.is_empty() {
            String::new()
        } else {
            format!("; took {}", recorded.took.join(","))
        }
    ))
}

/// `boss tenant published`'s verdict: the line and the exit code the
/// launcher's guard reads — 0 stamped (the date is the first word), 1
/// no stamp in this database. An unreadable database is the caller's
/// error (exit 2 in the verb), never one of these.
pub async fn published_verdict(stamps: &dyn PublishedStamps) -> Result<(String, i32)> {
    Ok(match stamps.published().await? {
        Some(p) => (p.render(), 0),
        None => (
            "no tenant publish stamped in this database: the launcher publishes on the next boot; \
             boss tenant publish <dir> publishes now"
                .to_string(),
            1,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn stamp(tenant: &str, at: &str, took: &[&str]) -> Stamp {
        Stamp {
            tenant_id: tenant.to_string(),
            published_at: Utc.from_utc_datetime(
                &chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S").unwrap(),
            ),
            published_by: "automation:tenant-seed".to_string(),
            boss_commit: "478231fb".to_string(),
            took: took.iter().map(|s| s.to_string()).collect(),
            writes: 12,
        }
    }

    fn new_stamp(took: &[&str]) -> NewStamp {
        NewStamp {
            tenant_id: "acme".into(),
            boss_commit: "478231fb".into(),
            took: took.iter().map(|s| s.to_string()).collect(),
            writes: 12,
        }
    }

    // THE READ'S TWO ADAPTERS RUN ONE SUITE (backlog be459ab9, design
    // 3036296f mechanism C). The launcher's guard reads PgStamps; every
    // verdict test above and below reads InMemoryStamps. Until this car
    // the two were held to each other by two tests written twice — one
    // per adapter, the same fixture typed in each — which is the drift
    // the suite exists to refuse. Each case is now written once and run
    // against both, and the census pin
    // (boss-testing/tests/every_port_with_two_adapters_runs_one_suite.rs)
    // reads this module as PublishedStamps' suite. It is a module of its
    // own because the pin reads the module that invokes the suite, and
    // the door test below names the jobs API's adapters, not these.
    mod the_read_adapters_agree {
        use super::*;

        /// How a case writes a stamp into each adapter's world: the double
        /// pushes the row; the Postgres read's table is filled through the
        /// jobs API's own Pg adapter, the one writer it has in production.
        #[async_trait]
        trait Seed: PublishedStamps {
            async fn seed(&self, stamp: Stamp);
        }

        #[async_trait]
        impl Seed for InMemoryStamps {
            async fn seed(&self, stamp: Stamp) {
                self.push(stamp);
            }
        }

        #[async_trait]
        impl Seed for PgStamps {
            async fn seed(&self, stamp: Stamp) {
                use boss_jobs::tenant_publishes::{PgTenantPublishes, TenantPublishes};
                PgTenantPublishes::new(self.0.clone())
                    .record(&stamp)
                    .await
                    .unwrap();
            }
        }

        async fn an_unstamped_store_answers_none_and_exit_1<S: Seed>(s: &S, adapter: &str) {
            assert_eq!(s.published().await.unwrap(), None, "{adapter}");
            let (line, code) = published_verdict(s).await.unwrap();
            assert_eq!(code, 1, "{adapter}");
            assert!(
                line.starts_with("no tenant publish stamped"),
                "{adapter}: {line}"
            );
        }

        async fn the_first_publish_is_the_stamp_and_later_ones_are_counted<S: Seed>(
            s: &S,
            adapter: &str,
        ) {
            // Recorded out of order: the FIRST by date is the stamp, not
            // the first written.
            s.seed(stamp("acme", "2026-09-19T08:00:00", &["agents"]))
                .await;
            s.seed(stamp("acme", "2026-09-18T19:00:00", &[])).await;
            let p = s.published().await.unwrap().unwrap();
            assert_eq!(
                p.first,
                stamp("acme", "2026-09-18T19:00:00", &[]),
                "{adapter}"
            );
            assert_eq!(p.count, 2, "{adapter}");
            assert_eq!(
                p.last_at,
                stamp("acme", "2026-09-19T08:00:00", &[]).published_at,
                "{adapter}"
            );
            let (line, code) = published_verdict(s).await.unwrap();
            assert_eq!(code, 0, "{adapter}");
            assert!(
                line.starts_with("2026-09-18T19:00:00Z tenant acme"),
                "{adapter}: {line}"
            );
        }

        /// Two publishes in the same second: the first WRITTEN is the stamp
        /// — the SQL's `ORDER BY published_at, id` and the double's stable
        /// sort, stated once for both.
        async fn a_tie_on_the_date_answers_the_first_written<S: Seed>(s: &S, adapter: &str) {
            s.seed(stamp("first", "2026-09-18T19:00:00", &[])).await;
            s.seed(stamp("second", "2026-09-18T19:00:00", &[])).await;
            let p = s.published().await.unwrap().unwrap();
            assert_eq!(p.first.tenant_id, "first", "{adapter}");
            assert_eq!(p.count, 2, "{adapter}");
        }

        boss_testing::adapters_agree! {
            adapters {
                in_memory => (InMemoryStamps::default(), ()),
                postgres => {
                    let db = boss_testing::TestDb::new().await;
                    (PgStamps::from_pool(db.pool.clone()), db)
                },
            }
            cases {
                an_unstamped_store_answers_none_and_exit_1,
                the_first_publish_is_the_stamp_and_later_ones_are_counted,
                a_tie_on_the_date_answers_the_first_written,
            }
        }
    }

    #[tokio::test]
    async fn renders_the_date_first() {
        // tenant-launch.sh reads `${stamp%% *}` as the date.
        let s = InMemoryStamps::default();
        s.push(stamp("acme", "2026-09-18T19:00:00", &[]));
        let (line, code) = published_verdict(&s).await.unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            line,
            "2026-09-18T19:00:00Z tenant acme published by automation:tenant-seed (boss 478231fb); \
             1 publish, last 2026-09-18T19:00:00Z"
        );
        s.push(stamp("acme", "2026-09-19T08:00:00", &["agents"]));
        let (line, _) = published_verdict(&s).await.unwrap();
        assert!(
            line.ends_with("2 publishes, last 2026-09-19T08:00:00Z"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn the_stamp_line_is_the_recorded_stamp_and_the_fact_rides_with_it() {
        let s = InMemoryStamps::default();
        let line = stamp_after_publish(&s, &new_stamp(&["employees", "agents"]), "agent-claude")
            .await
            .unwrap();
        assert!(
            line.starts_with("stamped: tenant acme publish recorded in tenant_publishes at ")
                && line.ends_with(" by agent-claude (boss 478231fb); took employees,agents"),
            "{line}"
        );
        assert_eq!(s.published().await.unwrap().unwrap().count, 1);
        // The row is a projection; the FACT is the event recorded
        // with it (backlog dbdc4d31): one tenant.published carrying
        // the stamp's columns.
        let events = s.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TENANT_PUBLISHED);
        assert_eq!(events[0].payload["_actor"], "agent-claude");
    }

    /// The adapter the binary runs, against the REAL door (backlog
    /// 42da8bd2): HttpStamps posts to `/api/tenant/publishes`, signed as
    /// the actor, and prints what the door answered — a refusal is an
    /// error naming the status, never a quiet "not stamped".
    #[tokio::test(flavor = "multi_thread")]
    async fn http_stamps_record_through_the_jobs_door_signed_as_the_actor() {
        let repo =
            std::sync::Arc::new(boss_jobs::tenant_publishes::InMemoryTenantPublishes::default());
        let app = boss_jobs::tenant_publishes::http::router(
            boss_jobs::tenant_publishes::http::TenantPublishesApiState { repo: repo.clone() },
        )
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Routed the way `--gateway` routes, so the client stamps no
        // token: this test never reads the process's mounted Secret.
        let http = HttpStamps::new(&crate::tenant_publish::Bases::resolve(Some(&base))).unwrap();
        assert!(
            http.client
                .get(&base)
                .build()
                .unwrap()
                .headers()
                .get(boss_core::machine_token::HEADER)
                .is_none(),
            "a stamp through a gateway carries no machine token (backlog 2ee29275)"
        );
        // NO DEADLINE OF THE TEST'S OWN (backlog ec131700). `new` carries
        // thirty seconds, a production bound against a stuck jobs door;
        // sent through it, this was also a test of the runner. On gate-run
        // 34031e7c (2026-10-07, two other gates beside it) the first POST
        // read "operation timed out" from a stub in this same process, on
        // a car that touches nothing here. Reproduced by holding the
        // stub's first answer for thirty-one seconds: red through `new`,
        // green here. `new` is still what is judged above, for the token,
        // and it still supplies the base; the sends below ask who the
        // stamp is signed as and what a refusal reads like, so the answer
        // is the condition waited on: the same unstamped client, no timer.
        let http = HttpStamps {
            base: http.base,
            client: boss_core::machine_token::Client::unstamped(reqwest::Client::builder())
                .unwrap(),
        };
        let line = stamp_after_publish(&http, &new_stamp(&["departments"]), "agent-claude")
            .await
            .unwrap();
        assert!(
            line.contains(" by agent-claude (boss 478231fb); took departments"),
            "{line}"
        );
        let rows = repo.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.published_by, "agent-claude");
        assert_eq!(rows[0].1.payload["_actor"], "agent-claude");

        // A door that refuses is an error that names the refusal.
        let mut bad = new_stamp(&[]);
        bad.tenant_id = String::new();
        let err = stamp_after_publish(&http, &bad, "agent-claude")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("400"), "{err:#}");
        assert_eq!(repo.rows().len(), 1);
    }
}
