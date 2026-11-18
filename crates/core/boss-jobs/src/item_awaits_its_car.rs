//! A BACKLOG ITEM'S BUILD WAITS FOR ITS CAR'S PROOF (backlog 81dc0c39).
//!
//! An item closes when the car that answers it closes `merged`: the
//! arrival rule `complete-feedback-branch-on-car-merged` completes the
//! item's `build`, and a car's `merged` terminal waits on `proven`. So
//! between the merge and the proof the item is correctly open, and
//! nothing said why. It read as forgotten and was completed by hand —
//! items 2682c418 and bed07fe8 closed as delivered on 2026-11-06 while
//! their cars (a0c7fc14, d37e17b7) stood at `proven` READY with their
//! probes never run, and the proof link was lost from the item.
//!
//! WHICH HALF WAS MISSING, MEASURED (2026-11-10). Car 2dd4b8fd's
//! `jobs.job.closed` carried `outcome: merged` at 12:06:32, so the emit
//! site was sound and the rule fired as designed — 51 minutes after item
//! 21217d8a's build had been completed by hand. The refusal belongs on
//! the item side.
//!
//! ONE PREDICATE (CLAUDE.md §9a). "Awaits its proof" is the shed's own
//! [`crate::regions::awaiting_proof`] — an open car whose live step is
//! `proven` — and the words are the shed's own (`borders::shed_words`).
//! A car that is proven and waits only on its landed review is NOT held
//! here: its proof is on the record, so a hand completion loses
//! nothing. A closed car is never held either, which is what lets the
//! arrival rule's own completion — it runs after the close — through.
//!
//! WHY A CONTRACT AND NOT A BRANCH IN THE STEP DOOR (CLAUDE.md §9): the
//! `build` row declares `completion_contract = NAME`, so the rule is the
//! protocol's, versioned with it; in-flight items stay on the version
//! they were admitted under until `boss job convert` moves them, which
//! `protocol_conversion` admits because their `build` is not yet done.

use crate::car::{ALSO_ANSWERS, BACKLOG_ITEM};
use crate::completion_contract::{CompletionContract, Unread};
use crate::port::{JobFilter, JobsRepository};
use anyhow::Result;
use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Step};
use serde_json::Value;
use std::sync::Arc;

/// The contract's name, as the `build` row declares it.
pub const NAME: &str = "item-awaits-its-car-v1";

/// One page of the car read. The reads page until the rows held agree
/// with `total`, so this bounds a page, never the answer.
const PAGE: i64 = 200;

pub struct ItemAwaitsItsCar<R: JobsRepository> {
    pub jobs: Arc<R>,
}

impl<R: JobsRepository> ItemAwaitsItsCar<R> {
    /// Every open car matching `filter`, paged until the rows agree with
    /// the total — a limit is not a filter.
    async fn open_cars(&self, filter: JobFilter) -> Result<Vec<Job>> {
        let filter = JobFilter {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..filter
        };
        let mut cars = Vec::new();
        loop {
            let (page, total) = self
                .jobs
                .list_jobs(&filter, PAGE, cars.len() as i64)
                .await
                .map_err(|why| Unread::Linked(why.to_string()))?;
            let empty = page.is_empty();
            cars.extend(page);
            if cars.len() as i64 >= total {
                return Ok(cars);
            }
            if empty {
                return Err(Unread::Linked(format!(
                    "the open cars read {} of {total} rows and then an empty page",
                    cars.len()
                ))
                .into());
            }
        }
    }

    /// The open cars that answer `item`, with their steps: the one whose
    /// `backlog_item` it is, and each whose `also_answers` lists it —
    /// exactly the two links the arrival rule follows.
    async fn answering(&self, item: &JobId) -> Result<Vec<(Job, Vec<Step>)>> {
        let id = item.to_string();
        let mut cars = self
            .open_cars(JobFilter {
                metadata_contains: Some(serde_json::json!({ BACKLOG_ITEM: id })),
                ..JobFilter::default()
            })
            .await?;
        let listing = self
            .open_cars(JobFilter {
                metadata_has: Some(ALSO_ANSWERS.into()),
                ..JobFilter::default()
            })
            .await?;
        cars.extend(listing.into_iter().filter(|car| {
            car.metadata
                .get(ALSO_ANSWERS)
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id.as_str())))
        }));
        cars.sort_by_key(|car| *car.id.inner().as_uuid());
        cars.dedup_by_key(|car| car.id);
        let ids: Vec<JobId> = cars.iter().map(|car| car.id).collect();
        let steps = self
            .jobs
            .list_steps_for_jobs(&ids)
            .await
            .map_err(|why| Unread::Linked(why.to_string()))?;
        Ok(cars
            .into_iter()
            .map(|car| {
                let own = steps
                    .iter()
                    .filter(|s| s.job_id == car.id)
                    .cloned()
                    .collect();
                (car, own)
            })
            .collect())
    }
}

/// The refusal, naming the car, where it stands in the shed, and what
/// will complete the build instead.
fn refusal(car: &Job) -> String {
    let id = car.id.to_string();
    let branch = car
        .metadata
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or(car.title.as_str());
    format!(
        "car {} ({branch}) answers this item and awaits its proof — {}. Its close \
         completes this build (rule complete-feedback-branch-on-car-merged); completed \
         by hand now, the item would read delivered before its probe ran. Prove or \
         settle car {id} first.",
        &id[..8],
        crate::borders::shed_words(&car.metadata),
    )
}

#[async_trait]
impl<R: JobsRepository + 'static> CompletionContract for ItemAwaitsItsCar<R> {
    async fn validate(
        &self,
        job: &Job,
        _step: &Step,
        _actor: &boss_policy_client::User,
    ) -> Result<()> {
        let cars = self.answering(&job.id).await?;
        match crate::regions::awaiting_proof(&cars).first() {
            Some((car, _)) => anyhow::bail!(refusal(car)),
            None => Ok(()),
        }
    }
}
