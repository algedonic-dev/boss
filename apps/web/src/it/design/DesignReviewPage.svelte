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
  import { href } from '../../router';

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
  };

  let docs = $state<ReadonlyArray<DesignDoc>>([]);
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

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const docsResp = await fetch('/api/design/docs');
      if (!docsResp.ok) throw new Error(`docs: HTTP ${docsResp.status}`);
      docs = (await docsResp.json()) as DesignDoc[];

      // Look up open design-doc-review Jobs. Subject is the
      // identity-first {subject_kind: 'custom', id: <doc-path>};
      // jobs-api supports ?kind= + ?status= filters.
      const jobsResp = await fetch(
        '/api/jobs?kind=design-doc-review&status=open&limit=200',
      );
      if (!jobsResp.ok) throw new Error(`jobs: HTTP ${jobsResp.status}`);
      const jobsBody = await jobsResp.json();
      const jobs: Array<{
        id: string;
        title: string;
        status: string;
        opened_on: string;
        subject: { id?: string };
      }> = Array.isArray(jobsBody) ? jobsBody : (jobsBody.data ?? []);
      const byPath: Record<string, OpenReviewJob> = {};
      for (const j of jobs) {
        const p = j.subject?.id;
        if (!p) continue;
        byPath[p] = { id: j.id, status: j.status, opened_on: j.opened_on, title: j.title };
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
      // (the JobKind's metadata_defaults template `{subject.id}`) — no
      // follow-up PUT. The old fill-in write lost read-overlay-write
      // races against dispatcher assignment and workforce completion,
      // and terminal-metadata immutability then sealed the empty value
      // (the 2026-07-14 "doc_path is empty" incident).
      await load();
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

<PageHeader title="Design review" subtitle="Open questions, pending decisions, ADRs" />

{#if loading}
  <p class="empty">Loading design docs…</p>
{:else if error}
  <p class="empty">Error: {error}</p>
{:else}
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
          <td>{doc.status}</td>
          <td>{doc.open_questions}</td>
          <td>{doc.pending_count}</td>
          <td>{relTime(doc.last_modified)}</td>
          <td>
            {#if review}
              <Link to={`/service/${review.id}`}>
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
  .design-table {
    width: 100%;
    border-collapse: collapse;
  }
  .design-table th,
  .design-table td {
    text-align: left;
    padding: 8px 12px;
    border-bottom: 1px solid var(--color-border, #e0e0e0);
    vertical-align: top;
  }
  .design-path {
    color: var(--color-fg-muted, #666);
    font-size: 12px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .empty {
    color: var(--color-fg-muted, #666);
    margin: 12px 0;
  }
</style>
