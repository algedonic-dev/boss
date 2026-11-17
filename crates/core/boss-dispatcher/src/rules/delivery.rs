//! Independent delivery is registry data. Handler authority and failure
//! retry budgets stay unchanged; unknown authoritative routing holds. A persisted stream floor divides old and new delivery;
//! event timestamps (including simulation time) never select the lane.
use super::registry::Registry;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cutover {
    pub lane: String,
    pub consumer: String,
    pub stream_created_ns: String,
    pub consumer_created_ns: String,
    pub created_at: String,
    pub after_stream_sequence: u64,
    pub members: BTreeMap<String, u32>,
}

/// A consumer's server position, separate from the domain event's instant.
#[derive(Clone, Debug)]
pub struct Position {
    pub lane: Option<String>,
    pub stream_created_ns: String,
    pub stream_sequence: u64,
}

pub fn eligible(
    lane: Option<&str>,
    receipt: Option<&Cutover>,
    position: &Position,
) -> Result<bool, String> {
    match lane {
        None => Ok(position.lane.is_none()),
        Some(lane) => {
            let receipt = receipt.ok_or_else(|| {
                format!("delivery lane {lane:?} has no committed cutover; holding delivery")
            })?;
            if receipt.lane != lane || receipt.stream_created_ns != position.stream_created_ns {
                return Err(format!(
                    "delivery lane {lane:?} stream identity differs from its cutover"
                ));
            }
            Ok(
                if position.stream_sequence <= receipt.after_stream_sequence {
                    position.lane.is_none()
                } else {
                    position.lane.as_deref() == Some(lane)
                },
            )
        }
    }
}

#[async_trait]
pub trait DeliveryRoutes: Send + Sync {
    async fn permitted(
        &self,
        names: &[String],
        position: Option<&Position>,
    ) -> Result<BTreeSet<String>, String>;
}

pub struct PgDeliveryRoutes(pub PgPool);

#[async_trait]
impl DeliveryRoutes for PgDeliveryRoutes {
    async fn permitted(
        &self,
        names: &[String],
        position: Option<&Position>,
    ) -> Result<BTreeSet<String>, String> {
        // One snapshot is essential: an active lane with no receipt holds,
        // including when this process still has the old default rule cached.
        let rows = sqlx::query("SELECT r.name, r.version, r.delivery_lane, l.receipt FROM dispatcher_rules r LEFT JOIN dispatcher_delivery_lanes l ON l.receipt->'members' ? r.name WHERE r.status = 'active' AND r.name = ANY($1)")
            .bind(names).fetch_all(&self.0).await.map_err(|e| format!("delivery routes unread: {e}"))?;
        let mut allowed = BTreeSet::new();
        for row in rows {
            let name: String = row.try_get("name").map_err(|e| e.to_string())?;
            let lane: Option<String> = row.try_get("delivery_lane").map_err(|e| e.to_string())?;
            let raw: Option<serde_json::Value> =
                row.try_get("receipt").map_err(|e| e.to_string())?;
            let receipt = raw
                .map(serde_json::from_value::<Cutover>)
                .transpose()
                .map_err(|e| format!("delivery cutover unread: {e}"))?;
            let version: i32 = row.try_get("version").map_err(|e| e.to_string())?;
            if let Some(receipt) = &receipt
                && (receipt.members.get(&name).copied() != u32::try_from(version).ok()
                    || lane.as_deref() != Some(receipt.lane.as_str()))
            {
                return Err(format!(
                    "rule {name:?} differs from its immutable lane membership/version; holding delivery"
                ));
            }
            let permit = match position {
                Some(position) => eligible(lane.as_deref(), receipt.as_ref(), position)?,
                None if lane.is_none() => true,
                None => return Err("independent delivery lanes require JetStream positions; log transport is unsupported, not a fallback".into()),
            };
            if permit {
                allowed.insert(name);
            }
        }
        Ok(allowed)
    }
}

pub fn lane_rules(registry: &Registry) -> BTreeMap<String, Vec<String>> {
    let mut lanes = BTreeMap::<String, Vec<String>>::new();
    for rule in registry.rules() {
        if let Some(lane) = &rule.delivery_lane {
            lanes
                .entry(lane.clone())
                .or_default()
                .push(rule.name.clone());
        }
    }
    for names in lanes.values_mut() {
        names.sort();
    }
    lanes
}

pub fn consumer_config(
    lane: &str,
    subjects: Vec<String>,
) -> async_nats::jetstream::consumer::pull::Config {
    let mut config = boss_nats::durable::consumer_config(
        &format!("dispatcher-lane-{lane}"),
        boss_nats::durable::coarse_filter_subjects(&subjects),
    );
    config.deliver_policy = async_nats::jetstream::consumer::DeliverPolicy::New;
    config
}

/// Called before pulling. A crash after server creation but before commit
/// recovers the untouched consumer's original floor; a used orphan refuses.
pub fn initial_cutover(
    lane: &str,
    members: BTreeMap<String, u32>,
    stream_created_ns: String,
    info: &async_nats::jetstream::consumer::Info,
) -> Result<Cutover, String> {
    if info.stream_name != boss_nats::durable::STREAM_NAME
        || info.name != format!("dispatcher-lane-{lane}")
    {
        return Err("lane consumer identity mismatch".into());
    }
    if info.delivered.consumer_sequence != 0
        || info.ack_floor.consumer_sequence != 0
        || info.delivered.stream_sequence != info.ack_floor.stream_sequence
        || info.num_ack_pending != 0
    {
        return Err(format!(
            "lane {lane:?} consumer has deliveries but no cutover receipt; refusing to invent an initial floor"
        ));
    }
    if info.config.deliver_policy != async_nats::jetstream::consumer::DeliverPolicy::New {
        return Err("independent lane must start new-only, never DeliverAll".into());
    }
    if info.config.description.as_deref() != Some(membership_description(&members).as_str()) {
        return Err(
            "lane consumer membership/version identity is absent or different; no orphan adoption"
                .into(),
        );
    }
    let created_at =
        chrono::DateTime::from_timestamp(info.created.unix_timestamp(), info.created.nanosecond())
            .ok_or_else(|| {
                "lane consumer creation time is outside the supported range".to_string()
            })?
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    Ok(Cutover {
        lane: lane.into(),
        consumer: info.name.clone(),
        stream_created_ns,
        consumer_created_ns: info.created.unix_timestamp_nanos().to_string(),
        created_at,
        after_stream_sequence: info.ack_floor.stream_sequence,
        members,
    })
}

/// What binding may do with a lane's consumer, decided from its receipt
/// alone, before any server call.
#[derive(Debug, PartialEq, Eq)]
pub enum LaneConsumer {
    /// No committed receipt: a new lane, whose consumer binding creates.
    Create,
    /// A committed receipt whose membership and stream still match: its
    /// consumer is only looked up, never recreated.
    Lookup(Cutover),
}

/// Backlog 960fabfc: `bind_lanes` used to get-or-create the consumer and
/// judge the receipt afterwards, so a bound lane whose consumer or stream
/// had gone got a fresh new-only consumer on its fixed name and was then
/// refused, orphaning that consumer on the server. The runner decided on
/// 2026-11-11 (run b529c357) that a lane consumer is never recreated, so
/// binding decides first: only a lane with no receipt may create.
pub fn lane_consumer(
    lane: &str,
    raw: Option<serde_json::Value>,
    members: &BTreeMap<String, u32>,
    stream_created_ns: &str,
) -> Result<LaneConsumer, String> {
    let Some(raw) = raw else {
        return Ok(LaneConsumer::Create);
    };
    let receipt: Cutover =
        serde_json::from_value(raw).map_err(|e| format!("lane cutover unread: {e}"))?;
    if &receipt.members != members {
        return Err(format!(
            "lane {lane:?} membership changed since its cutover; use a new lane, never rewrite its cutover"
        ));
    }
    if receipt.stream_created_ns != stream_created_ns {
        return Err(format!(
            "lane {lane:?} stream was recreated since its cutover; use a new lane, never rewrite its cutover"
        ));
    }
    Ok(LaneConsumer::Lookup(receipt))
}

/// Successful lanes and explicit isolated refusals from one reconciliation.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaneBindings {
    pub ready: Vec<Cutover>,
    pub refused: BTreeMap<String, String>,
}

pub async fn bind_lanes(
    pool: &PgPool,
    js: &async_nats::jetstream::Context,
    registry: &Registry,
) -> Result<LaneBindings, String> {
    let lanes = lane_rules(registry);
    if lanes.is_empty() {
        return Ok(LaneBindings::default());
    }
    // Ensured when the server says it is gone (backlog 2e45840a): a
    // missing stream here held every rules consumer stopped until a
    // reconnect. A recreated stream has a new identity, so every lane
    // bound to the old one is refused below, as its fence requires.
    let stream = boss_nats::durable::bound_stream(js, boss_nats::durable::STREAM_NAME)
        .await
        .map_err(|e| e.to_string())?;
    let stream_created_ns = stream
        .cached_info()
        .created
        .unix_timestamp_nanos()
        .to_string();
    let mut result = LaneBindings::default();
    for (lane, _names) in lanes {
        let bound = async {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        // Serialize only initialization of this named transport receipt.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("dispatcher-delivery-lane:{lane}"))
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        // Serialize the initial membership snapshot with every ordinary
        // registry write, before creating a consumer that could outlive us.
        sqlx::query("LOCK TABLE dispatcher_rules IN SHARE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let members: BTreeMap<String, u32> = registry
            .rules()
            .iter()
            .filter(|r| r.delivery_lane.as_deref() == Some(&lane))
            .map(|r| (r.name.clone(), r.version))
            .collect();
        let actual: serde_json::Value = sqlx::query_scalar("SELECT COALESCE(jsonb_object_agg(name, version), '{}'::jsonb) FROM dispatcher_rules WHERE status='active' AND delivery_lane=$1")
            .bind(&lane).fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
        if actual != serde_json::to_value(&members).map_err(|e| e.to_string())? {
            return Err(format!(
                "lane {lane:?} active membership/version changed before binding"
            ));
        }
        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT receipt FROM dispatcher_delivery_lanes WHERE name = $1")
                .bind(&lane)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        // Decided before any consumer call (backlog 960fabfc): a refused
        // lane creates nothing on the server.
        let action = lane_consumer(&lane, raw, &members, &stream_created_ns)?;
        let mut subjects: Vec<String> = registry
            .rules()
            .iter()
            .filter(|r| r.delivery_lane.as_deref() == Some(&lane))
            .filter_map(|r| r.event_pattern().map(|p| p.raw().to_string()))
            .collect();
        subjects.sort();
        subjects.dedup();
        let mut config = consumer_config(&lane, subjects);
        config.description = Some(membership_description(&members));
        let name = config.name.clone().unwrap_or_default();
        let consumer: async_nats::jetstream::consumer::Consumer<
            async_nats::jetstream::consumer::pull::Config,
        > = match &action {
            // get-or-create, not create: a crash after the server created
            // the consumer but before the receipt committed recovers it,
            // and `initial_cutover` refuses one that was already used.
            LaneConsumer::Create => tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stream.get_or_create_consumer(&name, config.clone()),
            )
            .await
            .map_err(|_| "lane consumer initialization exceeded two seconds".to_string())?
            .map_err(|e| e.to_string())?,
            LaneConsumer::Lookup(_) => tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stream.get_consumer(&name),
            )
            .await
            .map_err(|_| "lane consumer lookup exceeded two seconds".to_string())?
            .map_err(|e| format!(
                "lane {lane:?} consumer {name:?} is gone or unread ({e}); a bound lane's consumer is never recreated: retire the lane and bind a new one"
            ))?,
        };
        let info = consumer.cached_info();
        if info.stream_name != boss_nats::durable::STREAM_NAME
            || info.name != config.name.clone().unwrap_or_default()
            || info.config.deliver_policy != config.deliver_policy
            || info.config.ack_policy != config.ack_policy
            || info.config.ack_wait != config.ack_wait
            || info.config.max_deliver != config.max_deliver
            || info.config.filter_subjects != config.filter_subjects
            || info.config.description != config.description
            || !info.config.filter_subject.is_empty()
            || !info.config.backoff.is_empty()
        {
            return Err(format!(
                "lane {lane:?} consumer identity/configuration mismatch; no reset attempted"
            ));
        }
        let receipt = match action {
            LaneConsumer::Lookup(receipt) => {
                if receipt.consumer_created_ns != info.created.unix_timestamp_nanos().to_string() {
                    return Err(format!(
                        "lane {lane:?} consumer is not the one its cutover pins; use a new lane, never rewrite its cutover"
                    ));
                }
                receipt
            }
            LaneConsumer::Create => {
                let receipt = initial_cutover(&lane, members, stream_created_ns.clone(), info)?;
                record_cutover_in_tx(&mut tx, &receipt).await?;
                receipt
            }
        };
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok::<Cutover, String>(receipt)
        }.await;
        match bound {
            Ok(receipt) => result.ready.push(receipt),
            Err(error) => {
                result.refused.insert(lane, error);
            }
        }
    }
    Ok(result)
}

pub fn pg_routes(pool: PgPool) -> Arc<dyn DeliveryRoutes> {
    Arc::new(PgDeliveryRoutes(pool))
}

pub const DELIVERY_LANE_BOUND: &str = "dispatcher.delivery_lane.bound";

pub fn membership_description(members: &BTreeMap<String, u32>) -> String {
    serde_json::json!({"delivery_lane_members": members}).to_string()
}

async fn record_cutover_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    receipt: &Cutover,
) -> Result<bool, String> {
    let payload = boss_core::publisher::inject_actor(
        serde_json::json!({"cutover": receipt}),
        &boss_core::actor::ActorId::Automation("dispatcher".into()),
    );
    let event = boss_core::event::Event::new(
        "dispatcher",
        DELIVERY_LANE_BOUND,
        payload,
        boss_clock_client::wall_now(),
    );
    let inserted = sqlx::query("INSERT INTO dispatcher_delivery_lanes (name, receipt, event_id, recorded_at) VALUES ($1, $2, $3, $4) ON CONFLICT (name) DO NOTHING")
        .bind(&receipt.lane)
        .bind(serde_json::to_value(receipt).map_err(|e| e.to_string())?)
        .bind(event.id)
        .bind(event.timestamp)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?.rows_affected() == 1;
    if !inserted {
        let raw: serde_json::Value =
            sqlx::query_scalar("SELECT receipt FROM dispatcher_delivery_lanes WHERE name=$1")
                .bind(&receipt.lane)
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;
        if raw != serde_json::to_value(receipt).map_err(|e| e.to_string())? {
            return Err(
                "an existing delivery floor or membership/version cannot be replaced".into(),
            );
        }
        return Ok(false);
    }
    boss_events::outbox::record_event_in_tx(tx, &event)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (boss_testing::TestDb, Cutover) {
        let db = boss_testing::TestDb::new().await;
        let raw = super::super::registry::parse_raw("[[rule]]\nname=\"watch\"\non_event=\"jobs.estate.observed\"\ndelivery_lane=\"estate-watch\"\n[[rule.do]]\nhandler=\"record\"\n").unwrap().rules.remove(0);
        super::super::authoring::create_draft(&db.pool, &raw, None, "emp-test")
            .await
            .unwrap();
        super::super::authoring::publish(&db.pool, "watch", "emp-test")
            .await
            .unwrap();
        (
            db,
            Cutover {
                lane: "estate-watch".into(),
                consumer: "dispatcher-lane-estate-watch".into(),
                stream_created_ns: "1".into(),
                consumer_created_ns: "2".into(),
                created_at: "2026-10-31T09:00:00Z".into(),
                after_stream_sequence: 10,
                members: BTreeMap::from([("watch".into(), 1)]),
            },
        )
    }

    async fn record(pool: &PgPool, receipt: &Cutover) -> Result<bool, String> {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let inserted = record_cutover_in_tx(&mut tx, receipt).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(inserted)
    }

    #[tokio::test]
    async fn a_cutover_row_and_its_fact_conserve_the_same_receipt() {
        let (db, receipt) = fixture().await;
        assert!(record(&db.pool, &receipt).await.unwrap());
        let row: serde_json::Value =
            sqlx::query_scalar("SELECT receipt FROM dispatcher_delivery_lanes WHERE name=$1")
                .bind(&receipt.lane)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let fact: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind=$1")
                .bind(DELIVERY_LANE_BOUND)
                .fetch_optional(&db.pool)
                .await
                .unwrap();
        assert_eq!(row, serde_json::to_value(&receipt).unwrap());
        assert_eq!(
            fact.map(|p| p["cutover"].clone()),
            Some(row),
            "an authoritative floor cannot exist without its equal outbox fact"
        );
        let identity_matches: bool = sqlx::query_scalar("SELECT l.event_id=e.event_id AND l.recorded_at=e.timestamp FROM dispatcher_delivery_lanes l JOIN event_outbox e ON e.event_id=l.event_id WHERE l.name=$1")
            .bind(&receipt.lane).fetch_one(&db.pool).await.unwrap();
        assert!(identity_matches);
    }

    #[tokio::test]
    async fn concurrent_duplicate_cutovers_stage_one_fact() {
        let (db, receipt) = fixture().await;
        let (first, second) = tokio::join!(record(&db.pool, &receipt), record(&db.pool, &receipt));
        let mut inserted = vec![first.unwrap(), second.unwrap()];
        inserted.sort();
        assert_eq!(inserted, vec![false, true]);
        assert!(!record(&db.pool, &receipt).await.unwrap());
        let facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind=$1")
            .bind(DELIVERY_LANE_BOUND)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(facts, 1);
    }

    #[tokio::test]
    async fn a_fact_staging_refusal_rolls_back_the_cutover() {
        let (db, receipt) = fixture().await;
        sqlx::query("ALTER TABLE event_outbox ADD CONSTRAINT refuse_lane_fact CHECK (kind <> 'dispatcher.delivery_lane.bound')").execute(&db.pool).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let staged = record_cutover_in_tx(&mut tx, &receipt).await;
        tx.rollback().await.unwrap();
        assert!(
            staged.is_err(),
            "the forced outbox refusal must reach the same transaction that wrote the floor"
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_delivery_lanes")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn bound_membership_refuses_retire_rebind_change_and_new_members_atomically() {
        use super::super::{authoring, registry::parse_raw};
        let (db, receipt) = fixture().await;
        record(&db.pool, &receipt).await.unwrap();
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(r) FROM dispatcher_rules r WHERE name='watch' AND status='active'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        for assignment in [
            "status='retired'",
            "delivery_lane=NULL",
            "delivery_lane='another-lane'",
            "when_expr='false'",
        ] {
            let result = sqlx::query(&format!(
                "UPDATE dispatcher_rules SET {assignment} WHERE name='watch' AND status='active'"
            ))
            .execute(&db.pool)
            .await;
            assert!(
                result.is_err(),
                "bound modification must refuse: {assignment}"
            );
        }
        sqlx::query("UPDATE dispatcher_rules SET delivery_lane=delivery_lane WHERE name='watch' AND status='active'").execute(&db.pool).await.unwrap();
        let mut changed = parse_raw("[[rule]]\nname=\"watch\"\non_event=\"jobs.estate.observed\"\n[[rule.do]]\nhandler=\"record\"\n").unwrap().rules.remove(0);
        authoring::create_draft(&db.pool, &changed, None, "emp-test")
            .await
            .unwrap();
        assert!(
            authoring::publish(&db.pool, "watch", "emp-test")
                .await
                .is_err()
        );
        changed.name = "new-member".into();
        changed.delivery_lane = Some(receipt.lane.clone());
        authoring::create_draft(&db.pool, &changed, None, "emp-test")
            .await
            .unwrap();
        assert!(
            authoring::publish(&db.pool, "new-member", "emp-test")
                .await
                .is_err()
        );
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(r) FROM dispatcher_rules r WHERE name='watch' AND status='active'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            before, after,
            "failed publication cannot retire its predecessor"
        );
        let facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind=$1")
            .bind(DELIVERY_LANE_BOUND)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(facts, 1, "refusals cannot invent another binding fact");
        let names = vec!["watch".to_owned()];
        let routes = PgDeliveryRoutes(db.pool.clone());
        let position = Position {
            lane: None,
            stream_created_ns: "1".into(),
            stream_sequence: 11,
        };
        assert!(
            routes
                .permitted(&names, Some(&position))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            routes
                .permitted(
                    &names,
                    Some(&Position {
                        lane: Some(receipt.lane),
                        ..position
                    })
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The decision `bind_lanes` takes BEFORE any consumer call (backlog
    /// 960fabfc). It used to get-or-create the consumer first and judge
    /// the receipt after, so a lane whose consumer or stream had gone got
    /// a fresh new-only consumer on its name and was then refused,
    /// leaving that consumer on the server accumulating unacked state.
    /// Only a lane with no receipt may create; one with a receipt is
    /// looked up or refused, and a moved stream or membership refuses
    /// with no server call at all.
    #[test]
    fn only_a_lane_with_no_receipt_may_create_its_consumer() {
        let members = BTreeMap::from([("watch".to_string(), 1)]);
        let receipt = Cutover {
            lane: "estate-watch".into(),
            consumer: "dispatcher-lane-estate-watch".into(),
            stream_created_ns: "1".into(),
            consumer_created_ns: "2".into(),
            created_at: "2026-10-31T09:00:00Z".into(),
            after_stream_sequence: 10,
            members: members.clone(),
        };
        let raw = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            lane_consumer("estate-watch", None, &members, "1"),
            Ok(LaneConsumer::Create),
            "a new lane binds by creating its consumer"
        );
        assert_eq!(
            lane_consumer("estate-watch", Some(raw.clone()), &members, "1"),
            Ok(LaneConsumer::Lookup(receipt.clone())),
            "a bound lane's consumer is only looked up"
        );
        let recreated = lane_consumer("estate-watch", Some(raw.clone()), &members, "9");
        assert!(
            recreated
                .as_ref()
                .is_err_and(|e| e.contains("stream was recreated")),
            "a recreated stream refuses before any consumer call: {recreated:?}"
        );
        let moved = BTreeMap::from([("watch".to_string(), 2)]);
        let changed = lane_consumer("estate-watch", Some(raw), &moved, "1");
        assert!(
            changed
                .as_ref()
                .is_err_and(|e| e.contains("membership changed")),
            "changed membership refuses before any consumer call: {changed:?}"
        );
        assert!(
            lane_consumer(
                "estate-watch",
                Some(serde_json::json!({"lane": 1})),
                &members,
                "1"
            )
            .is_err_and(|e| e.starts_with("lane cutover unread")),
            "an unreadable receipt refuses, never falls back to creating"
        );
    }

    #[tokio::test]
    async fn initial_binding_serializes_with_an_authoring_write_and_rejects_a_stale_snapshot() {
        let (db, receipt) = fixture().await;
        let mut writer = db.pool.begin().await.unwrap();
        sqlx::query(
            "UPDATE dispatcher_rules SET delivery_lane=NULL WHERE name='watch' AND status='active'",
        )
        .execute(&mut *writer)
        .await
        .unwrap();
        let pool = db.pool.clone();
        let task = tokio::spawn(async move { record(&pool, &receipt).await });
        // A conflicting write owns the table's RowExclusive lock. Binding
        // cannot validate or publish a floor while that transaction is open.
        tokio::task::yield_now().await;
        writer.commit().await.unwrap();
        assert!(task.await.unwrap().is_err());
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_delivery_lanes")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let facts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind=$1")
            .bind(DELIVERY_LANE_BOUND)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(facts, 0);
    }
}
