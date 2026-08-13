<script lang="ts">
  // /it/design — design-doc review surface.
  //
  // Lists every design doc indexed by boss-docs-api with its live
  // open-question count + pending (recorded-but-unflushed) decisions
  // + the in-flight design-doc-review Job if one exists. Opens a fresh
  // design-doc-review Job on demand for any doc that doesn't already
  // have an open one.
  //
  // Replaces the in-app decision-tracker surface retired
  // 2026-05-03 — instead of bespoke decision-tracker tables, the
  // workflow is a Job whose review-design step (custom plugin) gates
  // on every open question having a recorded resolution. The Job
  // itself is the durable record; pending-decisions / ADR-extraction
  // continue to use the existing /api/design endpoints unchanged.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { navigate } from '../../router';

  type DesignDoc = {
    path: string;
    title: string;
    status: string;
    /// Questions currently parsed from the doc's ## Open questions.
    open_questions: number;
    /// Decisions recorded in review but not yet flushed to git.
    pending_count: number;
    word_count: number;
    last_modified: string;
  };

  type OpenReviewJob = {
    id: string;
    status: string;
    opened_on: string;
    title: string;
    /// The `review-design` step, when the Job has materialized one.
    /// Reading a design doc is the whole point of this Job, and the
    /// job page renders the doc in a panel beside a sidebar, a job
    /// header and a step list — so the link below goes straight to
    /// the full-page step surface instead. Optional because a Job
    /// caught mid-materialization has no steps yet; the link falls
    /// back to the job page rather than 404ing on a step id we made
    /// up.
    reviewStepId?: string;
  };

  /// Step kind backing the review surface (`step_plugins` row
  /// 'review-design', tier 0 of the design-doc-review Workflow).
  const REVIEW_STEP_KIND = 'review-design';

  /// Where a review Job should open. The focused step route renders
  /// outside AppShell — chrome bar on top, the whole panel below it
  /// for the document.
  function reviewHref(job: OpenReviewJob): string {
    return job.reviewStepId
      ? `/jobs/${job.id}/steps/${job.reviewStepId}`
      : `/service/${job.id}`;
  }

  type Rejection = {
    path: string;
    reason: string;
    first_seen_at: string;
    last_seen_at: string;
  };

  let docs = $state<ReadonlyArray<DesignDoc>>([]);
  // Docs on disk that are NOT in the list below, and why. Empty is the
  // healthy state. Without this the panel silently showed a partial
  // corpus: a rejected doc has no design_docs row, so its absence read
  // as "nobody wrote it" — which is how transactional-audit-log.md
  // stayed invisible for six days.
  let rejections = $state<ReadonlyArray<Rejection>>([]);
  let openReviewsByPath = $state<Record<string, OpenReviewJob | undefined>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);

  // System actor for opening review Jobs — same shape inventory-api
  // uses for its system-initiated Job opens.
  const SYSTEM_USER = JSON.stringify({
    id: 'system',
    role: 'platform-admin',
    access_tier: 'operator',
    territory_account_ids: [],
    direct_report_ids: [],
    department: null,
  });

  /// Whole days a doc has been out of the tracker. The age is what
  /// makes a rejection actionable — "failed" invites a shrug, "absent
  /// for 6 days" does not.
  function daysSince(iso: string): number {
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return 0;
    return Math.max(0, Math.floor((Date.now() - then) / 86_400_000));
  }

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const docsResp = await fetch('/api/design/docs');
      if (!docsResp.ok) throw new Error(`docs: HTTP ${docsResp.status}`);
      docs = (await docsResp.json()) as DesignDoc[];

      // Rejections are supplementary — they name docs the indexer
      // could not parse. If that call fails, the page still has
      // everything an operator came for, so degrade to an empty list
      // rather than replacing the whole surface with an error. (It
      // did throw here once, which blanked the page whenever the
      // route was unavailable.)
      rejections = await fetch('/api/design/rejections')
        .then((r) => (r.ok ? (r.json() as Promise<Rejection[]>) : []))
        .catch(() => []);

      // Look up open design-doc-review Jobs. Subject is the
      // identity-first {subject_kind: 'custom', id: <doc-path>};
      // jobs-api supports ?kind= + ?status= filters.
      const jobsResp = await fetch(
        '/api/jobs?kind=design-doc-review&status=open&limit=200',
      );
      if (!jobsResp.ok) throw new Error(`jobs: HTTP ${jobsResp.status}`);
      const jobsBody = await jobsResp.json();
      // The list endpoint enriches each Job with its steps
      // (boss-jobs http/jobs.rs), so the review step is already here —
      // no per-Job follow-up fetch to find it.
      const jobs: Array<{
        id: string;
        title: string;
        status: string;
        opened_on: string;
        subject: { id?: string };
        steps?: Array<{ id: string; kind: string }>;
      }> = Array.isArray(jobsBody) ? jobsBody : (jobsBody.data ?? []);
      const byPath: Record<string, OpenReviewJob> = {};
      for (const j of jobs) {
        const p = j.subject?.id;
        if (!p) continue;
        byPath[p] = {
          id: j.id,
          status: j.status,
          opened_on: j.opened_on,
          title: j.title,
          reviewStepId: j.steps?.find((s) => s.kind === REVIEW_STEP_KIND)?.id,
        };
      }
      openReviewsByPath = byPath;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function openReview(doc: DesignDoc): Promise<void> {
    const body = {
      kind: 'design-doc-review',
      // Identity-first Subject: the doc path IS the subject id. The
      // pre-2026-06-13 {custom_kind, ref_id} shape 422s ("missing
      // field `id`") — this page shipped before that migration and
      // the button was dead until 2026-07-06.
      subject: {
        subject_kind: 'custom',
        id: doc.path,
      },
      title: `Review: ${doc.title}`,
      owner_id: 'system',
      priority: 'standard',
      status: 'open',
      metadata: {
        doc_path: doc.path,
        doc_title: doc.title,
      },
      tags: ['design-review'],
    };
    try {
      const resp = await fetch('/api/jobs', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'x-boss-user': SYSTEM_USER,
        },
        body: JSON.stringify(body),
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
      // doc_path is stamped at materialization from the Job's subject
      // (the Workflow's metadata_defaults template `{subject.id}`) — no
      // follow-up PUT. The old fill-in write lost read-overlay-write
      // races against dispatcher assignment and workforce completion,
      // and terminal-metadata immutability then sealed the empty value
      // (the 2026-07-14 "doc_path is empty" incident).
      await load();
      // Open the review where it is readable. Creating the Job and
      // dropping the operator back on a table row means the next
      // click lands on the job page, which renders the document in a
      // panel beside the sidebar and step list — the reason reviewing
      // a doc in-app felt cramped. If the Job has not materialized
      // its steps yet, reviewHref falls back to the job page.
      const opened = openReviewsByPath[doc.path];
      if (opened) navigate(reviewHref(opened));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void load();
  });

  // David's distinction (2026-07-08): a doc is "in review & discussion"
  // when its status says so OR anything actionable is attached (parsed
  // open questions, unflushed decisions, an open review Job). Everything
  // else — living references, approved/shipped/superseded designs — is
  // settled: nobody is acting on it, and showing it as in-review was a
  // lie the old status parser told (living → in-review collapse).
  function underReview(doc: DesignDoc): boolean {
    return (
      doc.status === 'draft' ||
      doc.status === 'in-review' ||
      doc.status === 'reopened' ||
      doc.open_questions > 0 ||
      doc.pending_count > 0 ||
      openReviewsByPath[doc.path] !== undefined
    );
  }

  const reviewing = $derived(docs.filter(underReview));
  const settled = $derived(docs.filter((d) => !underReview(d)));

  function relTime(iso: string): string {
    const d = new Date(iso);
    const now = new Date();
    const days = Math.floor((now.getTime() - d.getTime()) / 86_400_000);
    if (days < 1) return 'today';
    if (days === 1) return '1d ago';
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }
</script>

<PageHeader
  eyebrow="System Model · Design review"
  title="Design review"
  subtitle="Open questions, pending decisions, ADRs"
/>

{#if loading}
  <p class="empty">Loading design docs…</p>
{:else if error}
  <p class="design-error">Error: {error}</p>
{:else}
  {#if rejections.length > 0}
    <Section title={`Not indexed (${rejections.length})`} wide>
      <p class="reject-lede">
        These files are in <code>docs/design/</code> but are <strong>not</strong>
        in the lists below — the reindexer refused them. Until each is
        fixed, this panel is showing an incomplete corpus.
      </p>
      <table class="design-table">
        <thead>
          <tr><th>Doc</th><th>Invisible for</th><th>Why</th></tr>
        </thead>
        <tbody>
          {#each rejections as r (r.path)}
            <tr>
              <td><code>{r.path}</code></td>
              <td class="reject-age">
                {daysSince(r.first_seen_at)}
                {daysSince(r.first_seen_at) === 1 ? 'day' : 'days'}
              </td>
              <td class="reject-reason">{r.reason}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </Section>
  {/if}

  <Section title={`In review & discussion (${reviewing.length})`} wide>
    {#if reviewing.length === 0}
      <p class="empty">
        Nothing under discussion — every design doc is a settled
        reference. New questions land here when a doc adds
        <code>### Qn:</code> headings (status → reopened).
      </p>
    {:else}
      {@render docTable(reviewing, 'Open review Job')}
    {/if}
  </Section>

  <Section title={`Living references & settled (${settled.length})`} wide>
    {@render docTable(settled, 'Reopen discussion')}
  </Section>
{/if}

{#snippet docTable(rows: ReadonlyArray<DesignDoc>, buttonLabel: string)}
  <table class="design-table">
    <thead>
      <tr>
        <th>Doc</th>
        <th>Status</th>
        <th>Open Qs</th>
        <th>Pending decisions</th>
        <th>Last modified</th>
        <th>Review</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as doc (doc.path)}
        {@const review = openReviewsByPath[doc.path]}
        <tr>
          <td>
            <strong>{doc.title}</strong>
            <div class="design-path">{doc.path}</div>
          </td>
          <td class="design-status">{doc.status}</td>
          <td>{doc.open_questions}</td>
          <td>{doc.pending_count}</td>
          <td class="design-when">{relTime(doc.last_modified)}</td>
          <td>
            {#if review}
              <Link to={reviewHref(review)}>
                In review — {review.status}
              </Link>
            {:else}
              <button class="wb-btn" type="button" onclick={() => openReview(doc)}>
                {buttonLabel}
              </button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

<style>
  /* Warning prose, not an empty-state: FOG at reading line-height. It was
     STATIC via `.empty`, which buried the one paragraph explaining why the
     corpus above is incomplete. */
  .reject-lede {
    color: var(--fog, #E8ECEF);
    line-height: 1.6;
    max-width: 720px;
    margin: 0 0 12px;
  }
  .reject-age {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  /* Body prose in a cell. 0.85rem was 11.9px at the 14px root — below the
     13px body floor — with cramped leading. */
  .reject-reason {
    font-size: 13px;
    line-height: 1.6;
    max-width: 60ch;
  }
  .design-table {
    width: 100%;
    border-collapse: collapse;
  }
  .design-table th,
  .design-table td {
    text-align: left;
    padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138);
    vertical-align: top;
    font-variant-numeric: tabular-nums;
  }
  /* Column labels are instrument text: DM Mono caps in STATIC, not bold
     browser-default headers competing with the rows. Yard-board idiom. */
  .design-table th {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    font-weight: 400;
    letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
    color: var(--static, #7A838C);
  }
  .design-table tr:last-child td {
    border-bottom: none;
  }
  .design-status {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--static, #7A838C);
    white-space: nowrap;
  }
  .design-when {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
    color: var(--static, #7A838C);
    white-space: nowrap;
  }
  .design-path {
    color: var(--static, #7A838C);
    font-size: 12px;
    font-family: var(--font-mono, ui-monospace, monospace);
    margin-top: 2px;
  }
  /* Inline literals (paths, `### Qn:` markers) in the system mono, pinned
     to 12px — bare <code> falls into the browser's monospace-shrink. */
  code {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
  }
  .empty {
    color: var(--static, #7A838C);
    margin: 12px 0;
    line-height: 1.5;
  }
  .design-error {
    color: var(--err, #e2685c);
    margin: 12px 0;
  }
</style>
