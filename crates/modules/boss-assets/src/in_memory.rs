//! In-memory adapter for the `AssetsRepository` port.
//!
//! Used by tests and by dev/demo environments that don't need persistence.
//! Keeps events in a per-asset Vec and recomputes current state on read.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::{
    AssetsError, AssetsRepository, BatchAppendStats, model_named,
    refuse_an_actor_that_reads_back_as_another, refuse_negative_page, refuse_nul_in_event,
};
use crate::project::project;
use crate::types::{AssetCurrentState, AssetEvent, AssetId};

#[derive(Default)]
pub struct InMemoryAssets {
    inner: Mutex<State>,
    /// The catalog models this store holds, when one is declared
    /// (`with_models`). `None` is a double with no catalog wired, which
    /// admits every sku — the same "permissive when no registry is
    /// wired" this crate's HTTP layer keeps for its Class registry. The
    /// adapters-agree suite declares the catalog Postgres holds, and a
    /// declared catalog refuses a model it lacks `UnknownModel`, as
    /// Postgres's foreign key does (backlog be459ab9).
    models: Option<HashSet<String>>,
}

#[derive(Default)]
struct State {
    events: HashMap<AssetId, Vec<AssetEvent>>,
    seen_ids: HashSet<String>,
}

/// The order a log is kept and read in: by day, one day's events in
/// byte order of id — the order `project` folds them in.
fn by_day_then_id(a: &AssetEvent, b: &AssetEvent) -> std::cmp::Ordering {
    a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0))
}

impl InMemoryAssets {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store whose catalog holds exactly `models`: an event naming any
    /// other sku is refused `UnknownModel`.
    pub fn with_models(models: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            inner: Mutex::default(),
            models: Some(models.into_iter().map(Into::into).collect()),
        }
    }

    /// Seed from a prebuilt list of events (for demo data). Events are
    /// grouped by asset id; duplicates are rejected.
    pub fn with_events(events: impl IntoIterator<Item = AssetEvent>) -> Result<Self, AssetsError> {
        let assets = Self::new();
        {
            let mut state = assets.inner.lock().expect("poisoned lock");
            for e in events {
                if !state.seen_ids.insert(e.id.0.clone()) {
                    return Err(AssetsError::DuplicateEvent(e.id.0.clone()));
                }
                state.events.entry(e.asset_id.clone()).or_default().push(e);
            }
            // Keep per-asset logs chronologically sorted.
            for log in state.events.values_mut() {
                log.sort_by(by_day_then_id);
            }
        }
        Ok(assets)
    }

    /// Refuse a model the declared catalog does not hold.
    fn refuse_unknown_model(&self, event: &AssetEvent) -> Result<(), AssetsError> {
        match (model_named(&event.kind), &self.models) {
            (Some(sku), Some(models)) if !models.contains(sku) => {
                Err(AssetsError::UnknownModel(sku.to_string()))
            }
            _ => Ok(()),
        }
    }

    /// Every asset's projection.
    fn projections(state: &State) -> Vec<AssetCurrentState> {
        state
            .events
            .iter()
            .filter_map(|(asset_id, events)| project(asset_id, events))
            .collect()
    }
}

/// Store an event already judged storable and new.
fn store(state: &mut State, event: AssetEvent) {
    state.seen_ids.insert(event.id.0.clone());
    let log = state.events.entry(event.asset_id.clone()).or_default();
    log.push(event);
    log.sort_by(by_day_then_id);
}

/// A key holding a NUL byte names nothing Postgres can hold, so a read
/// keyed by one is the miss it is on both adapters.
fn holds_nul(key: &str) -> bool {
    key.contains('\0')
}

#[async_trait]
impl AssetsRepository for InMemoryAssets {
    async fn append(&self, event: AssetEvent) -> Result<(), AssetsError> {
        refuse_nul_in_event(&event)?;
        refuse_an_actor_that_reads_back_as_another(&event)?;
        let mut state = self.inner.lock().expect("poisoned lock");
        if state.seen_ids.contains(&event.id.0) {
            return Err(AssetsError::DuplicateEvent(event.id.0.clone()));
        }
        self.refuse_unknown_model(&event)?;
        store(&mut state, event);
        Ok(())
    }

    /// All or nothing, as the Postgres adapter's one transaction is: the
    /// whole batch is judged before any of it is stored. The port's
    /// default loop stored every event ahead of a refusal (backlog
    /// be459ab9).
    async fn batch_append(&self, events: Vec<AssetEvent>) -> Result<BatchAppendStats, AssetsError> {
        for event in &events {
            refuse_nul_in_event(event)?;
            refuse_an_actor_that_reads_back_as_another(event)?;
        }
        let mut state = self.inner.lock().expect("poisoned lock");
        let mut fresh: Vec<AssetEvent> = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut stats = BatchAppendStats::default();
        for event in events {
            if state.seen_ids.contains(&event.id.0) || !taken.insert(event.id.0.clone()) {
                stats.duplicates += 1;
                continue;
            }
            self.refuse_unknown_model(&event)?;
            fresh.push(event);
        }
        stats.inserted = fresh.len() as u64;
        for event in fresh {
            store(&mut state, event);
        }
        Ok(stats)
    }

    async fn events_for(&self, asset_id: &AssetId) -> Result<Vec<AssetEvent>, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.events.get(asset_id).cloned().unwrap_or_default())
    }

    async fn current_state(
        &self,
        asset_id: &AssetId,
    ) -> Result<Option<AssetCurrentState>, AssetsError> {
        let events = self.events_for(asset_id).await?;
        Ok(project(asset_id, &events))
    }

    async fn all_asset_ids(&self) -> Result<Vec<AssetId>, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.events.keys().cloned().collect())
    }

    async fn list_asset_ids(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AssetId>, i64), AssetsError> {
        refuse_negative_page(limit, offset)?;
        let state = self.inner.lock().expect("poisoned lock");
        let mut all: Vec<AssetId> = state.events.keys().cloned().collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let total = all.len() as i64;
        Ok((page(all, limit, offset), total))
    }

    async fn list_assets(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<AssetCurrentState>, i64), AssetsError> {
        refuse_negative_page(limit, offset)?;
        let state = self.inner.lock().expect("poisoned lock");
        let mut assets: Vec<AssetCurrentState> = Self::projections(&state)
            .into_iter()
            .filter(|cs| {
                account_id.is_none_or(|p| {
                    cs.holder_kind.as_deref() == Some("account")
                        && cs.holder_id.as_deref() == Some(p)
                })
            })
            .collect();
        // Newest first; one day's assets in byte order of id. The tie
        // order was the HashMap's until backlog be459ab9 (and Postgres's
        // heap order), and a day is a common tie.
        assets.sort_by(|a, b| {
            b.last_event_at
                .cmp(&a.last_event_at)
                .then_with(|| a.asset_id.0.cmp(&b.asset_id.0))
        });
        let total = assets.len() as i64;
        Ok((page(assets, limit, offset), total))
    }

    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(Self::projections(&state)
            .iter()
            .filter(|cs| cs.sku.as_deref() == Some(sku) && !cs.phase.is_decommissioned())
            .count() as u64)
    }

    /// The open tickets of every asset the account holds that is not
    /// decommissioned — what Postgres counts off `asset_open_tickets`.
    /// Until backlog be459ab9 the double counted every `ServiceJobOpened`
    /// EVENT not closed anywhere, a decommissioned asset's and a
    /// redelivered open's included.
    async fn open_ticket_count_for_account(&self, account_id: &str) -> Result<u64, AssetsError> {
        if holds_nul(account_id) {
            return Ok(0);
        }
        let state = self.inner.lock().expect("poisoned lock");
        Ok(Self::projections(&state)
            .iter()
            .filter(|cs| {
                cs.holder_kind.as_deref() == Some("account")
                    && cs.holder_id.as_deref() == Some(account_id)
                    && !cs.phase.is_decommissioned()
            })
            .map(|cs| u64::from(cs.open_ticket_count))
            .sum())
    }

    async fn assets_summary(
        &self,
        today: chrono::NaiveDate,
    ) -> Result<crate::types::AssetsSummary, AssetsError> {
        use crate::types::{AssetLifecyclePhase, AssetsSummary, PhaseRollup, SkuRollup};
        let state = self.inner.lock().expect("poisoned lock");
        let assets = Self::projections(&state);
        let mut phase_map: HashMap<String, i64> = HashMap::new();
        let mut sku_map: HashMap<String, i64> = HashMap::new();
        let mut open_tickets_total: i64 = 0;
        let mut warranty_expiring_30d: i64 = 0;
        let horizon = today + chrono::Days::new(30);
        for cs in &assets {
            *phase_map.entry(cs.phase.as_str().to_string()).or_insert(0) += 1;
            if cs
                .warranty_through
                .is_some_and(|w| w >= today && w < horizon)
            {
                warranty_expiring_30d += 1;
            }
            if cs.phase.is_decommissioned() {
                continue;
            }
            open_tickets_total += i64::from(cs.open_ticket_count);
            // Only identified assets bucket into a per-model rollup; an
            // unidentified (Registered) asset has no model to count under.
            if let Some(sku) = &cs.sku {
                *sku_map.entry(sku.clone()).or_insert(0) += 1;
            }
        }
        let phase_counts: Vec<PhaseRollup> = AssetLifecyclePhase::ORDER
            .iter()
            .map(|p| PhaseRollup {
                phase: p.to_string(),
                count: phase_map.get(*p).copied().unwrap_or(0),
            })
            .collect();
        let total_systems: i64 = phase_counts.iter().map(|p| p.count).sum();
        let in_field_count: i64 = phase_counts
            .iter()
            .filter(|p| p.phase != "decommissioned")
            .map(|p| p.count)
            .sum();
        let mut sku_counts: Vec<SkuRollup> = sku_map
            .into_iter()
            .map(|(sku, count)| SkuRollup { sku, count })
            .collect();
        sku_counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.sku.cmp(&b.sku)));
        Ok(AssetsSummary {
            phase_counts,
            total_systems,
            in_field_count,
            open_tickets_total,
            sku_counts,
            warranty_expiring_30d,
        })
    }
}

/// `limit` items from `offset`, both already judged non-negative.
fn page<T>(all: Vec<T>, limit: i64, offset: i64) -> Vec<T> {
    all.into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssetCondition, AssetEventId, AssetEventKind, AssetLifecyclePhase, IntakeSource,
        WarrantyCoverage,
    };
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn evt(id: &str, serial: &str, ts: NaiveDate, kind: AssetEventKind) -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new(id),
            asset_id: AssetId::new(serial),
            ts,
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind,
        }
    }

    #[tokio::test]
    async fn append_then_read_back_events() {
        let assets = InMemoryAssets::new();
        let s = AssetId::new("SN-1");
        assets
            .append(evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let events = assets.events_for(&s).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.0, "e1");
    }

    #[tokio::test]
    async fn duplicate_event_id_is_rejected() {
        let assets = InMemoryAssets::new();
        assets
            .append(evt(
                "e1",
                "A",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();
        let err = assets
            .append(evt(
                "e1",
                "B",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, AssetsError::DuplicateEvent(_)));
    }

    #[tokio::test]
    async fn unknown_asset_id_returns_empty_log_and_none_state() {
        let assets = InMemoryAssets::new();
        let unknown = AssetId::new("ghost");
        assert!(assets.events_for(&unknown).await.unwrap().is_empty());
        assert!(assets.current_state(&unknown).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn current_state_projects_from_appended_events() {
        let assets = InMemoryAssets::new();
        let s = AssetId::new("SN-1");
        for (id, ts, kind) in [
            (
                "e1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            (
                "e2",
                d(2026, 1, 14),
                AssetEventKind::PutAway { bin: "A-01".into() },
            ),
            (
                "e3",
                d(2026, 2, 1),
                AssetEventKind::Sold {
                    account_id: "account-1".into(),
                    price_cents: 8_500_000,
                    currency: "USD".into(),
                    order_id: None,
                    condition: AssetCondition::new("new"),
                },
            ),
            (
                "e4",
                d(2026, 2, 10),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-1".into(),
                },
            ),
            (
                "e5",
                d(2026, 2, 10),
                AssetEventKind::WarrantyStarted {
                    through: d(2028, 2, 10),
                    coverage: WarrantyCoverage::new("standard"),
                },
            ),
        ] {
            assets.append(evt(id, "SN-1", ts, kind)).await.unwrap();
        }

        let state = assets.current_state(&s).await.unwrap().unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(state.holder_id.as_deref(), Some("account-1"));
        assert_eq!(state.warranty_through, Some(d(2028, 2, 10)));
    }

    #[tokio::test]
    async fn all_asset_ids_returns_every_known_id() {
        let assets = InMemoryAssets::new();
        assets
            .append(evt(
                "e1",
                "A",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();
        assets
            .append(evt(
                "e2",
                "B",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let mut ids = assets.all_asset_ids().await.unwrap();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(ids, vec![AssetId::new("A"), AssetId::new("B")]);
    }

    #[tokio::test]
    async fn with_events_bulk_loads_deterministically() {
        let events = vec![
            evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "e2",
                "SN-2",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ),
        ];
        let assets = InMemoryAssets::with_events(events).unwrap();
        assert_eq!(assets.all_asset_ids().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn events_are_returned_in_chronological_order() {
        let assets = InMemoryAssets::new();
        // Append out of order.
        assets
            .append(evt(
                "e2",
                "SN-1",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "c".into(),
                },
            ))
            .await
            .unwrap();
        assets
            .append(evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let events = assets.events_for(&AssetId::new("SN-1")).await.unwrap();
        assert_eq!(events[0].id.0, "e1");
        assert_eq!(events[1].id.0, "e2");
    }
}
