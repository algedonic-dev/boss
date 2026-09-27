//! EVERY ROW A FILTER MATCHES — the one paged read of the jobs list.
//!
//! A limit is not a filter. `list_jobs` answers ONE page and the total
//! behind it, and a reader that keeps the page and drops the total
//! answers a smaller question with no sign that it did: the station
//! load and a station's queue each read one `MAX_LIMIT` page of open
//! packets and bound the total as `_total` (backlog f71d1e81 — 465
//! open on 2026-09-27, so latent, and silent past 1000). The regions
//! read and the flights read had each already written this loop for
//! themselves; this is the one copy, so a reader that wants the whole
//! set has one door to it and no loop of its own to get wrong
//! (CLAUDE.md §9a).
//!
//! Paging by offset over a table that moves can hand the same row to
//! two pages, so each id is kept once. The read ends when the offset
//! reaches the total the last page reported, or on an empty page — a
//! table that shrank under the read ends it early with the rows that
//! still exist, which is the honest answer to "what matches now".

use boss_core::job::Job;

use crate::port::{JobFilter, JobsError, JobsRepository};

/// Every job `filter` matches, read `page` rows at a time until the
/// rows read reach the total the repository reports.
///
/// `page` is the caller's page size — the HTTP layer passes its own
/// `MAX_LIMIT`, the size a single list answer is clamped to — and a
/// non-positive one is read as 1 rather than looping on empty pages.
pub async fn list_every<R: JobsRepository + ?Sized>(
    repo: &R,
    filter: &JobFilter,
    page: i64,
) -> Result<Vec<Job>, JobsError> {
    let page = page.max(1);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rows: Vec<Job> = Vec::new();
    let mut offset: i64 = 0;
    loop {
        let (batch, total) = repo.list_jobs(filter, page, offset).await?;
        let got = i64::try_from(batch.len()).unwrap_or(i64::MAX);
        rows.extend(batch.into_iter().filter(|j| seen.insert(j.id.to_string())));
        offset = offset.saturating_add(got);
        if got == 0 || offset >= total {
            return Ok(rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryJobs;
    use boss_core::job::{JobStatus, Priority, Subject};

    fn job(i: usize) -> Job {
        let mut j = Job::new(
            "paged-kind",
            Subject::new("custom", format!("s-{i}")),
            format!("job {i}"),
            "emp-1",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        );
        j.status = JobStatus::Open;
        j
    }

    /// Seven packets read three at a time: one page holds three and
    /// says seven, and the read goes on until it holds all seven — the
    /// shape of 1001 open packets under a 1000-row page, small enough
    /// to see.
    #[tokio::test]
    async fn every_row_is_read_past_the_first_page() {
        let repo = InMemoryJobs::default();
        for i in 0..7 {
            repo.create_job(&job(i)).await.unwrap();
        }
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        // Control: one page is a smaller answer than the filter's.
        let (first, total) = repo.list_jobs(&filter, 3, 0).await.unwrap();
        assert_eq!((first.len(), total), (3, 7));

        let every = list_every(&repo, &filter, 3).await.unwrap();
        assert_eq!(every.len(), 7, "every open packet, not one page of them");
        let ids: std::collections::HashSet<String> =
            every.iter().map(|j| j.id.to_string()).collect();
        assert_eq!(ids.len(), 7, "each packet once");
    }

    /// An empty world is one read and no rows; a page size of zero is
    /// read as one rather than asking forever for empty pages.
    #[tokio::test]
    async fn an_empty_filter_and_a_zero_page_both_end() {
        let repo = InMemoryJobs::default();
        let none = list_every(&repo, &JobFilter::default(), 1000)
            .await
            .unwrap();
        assert!(none.is_empty());
        repo.create_job(&job(0)).await.unwrap();
        repo.create_job(&job(1)).await.unwrap();
        let two = list_every(&repo, &JobFilter::default(), 0).await.unwrap();
        assert_eq!(two.len(), 2);
    }
}
