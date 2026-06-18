<script lang="ts">
  // /it/design — design-doc review surface.
  //
  // Lists every design doc indexed by boss-docs-api with the parsed
  // pending_count (open questions not yet decided) + the in-flight
  // design-doc-review Job if one exists. Opens a fresh
  // design-doc-review Job on demand for any doc that doesn't already
  // have an open one.
  //
  // Replaces the in-app decision-tracker surface retired
  // 2026-05-03 — instead of bespoke decision-tracker tables, the
  // workflow is a Job whose review-design step (custom plugin) gates
  // on every open question having a recorded resolution. The Job
  // itself is the durable record; pending-decisions / ADR-extraction
  // continue to use the existing /api/design endpoints unchanged.
  import PageHeader from '../../ui/PageHeader.svelte';
  import Section from '../../ui/Section.svelte';
  import Link from '../../ui/Link.svelte';
  import { href } from '../../router';

  type DesignDoc = {
    path: string;
    title: string;
    status: string;
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

      // Look up open design-doc-review Jobs. Subject is
      // custom/design-doc/<doc-path>; jobs-api supports
      // ?kind= + ?status= filters.
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
        subject: { ref_id?: string };
      }> = Array.isArray(jobsBody) ? jobsBody : (jobsBody.data ?? []);
      const byPath: Record<string, OpenReviewJob> = {};
      for (const j of jobs) {
        const p = j.subject?.ref_id;
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
      subject: {
        subject_kind: 'custom',
        custom_kind: 'design-doc',
        ref_id: doc.path,
      },
      title: `Review: ${doc.title}`,
      owner_id: 'system',
      priority: 'normal',
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
      const created = await resp.json();
      // After the Job is created, its review-design step needs
      // doc_path stamped on its metadata. Materialization defaults
      // it to "" — fill it in by PUTting the step. (Future: the
      // dispatcher should do this from the Job's subject.ref_id.)
      const detailResp = await fetch(`/api/jobs/${created.id}`);
      if (detailResp.ok) {
        const detail = await detailResp.json();
        const reviewStep = (detail.steps ?? []).find(
          (s: { kind: string }) => s.kind === 'review-design',
        );
        if (reviewStep) {
          await fetch(`/api/jobs/${created.id}/steps/${reviewStep.id}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
              metadata: { ...reviewStep.metadata, doc_path: doc.path },
            }),
          });
        }
      }
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void load();
  });

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
  <Section title={`Design docs (${docs.length})`} wide>
    <table class="design-table">
      <thead>
        <tr>
          <th>Doc</th>
          <th>Status</th>
          <th>Open Qs</th>
          <th>Last modified</th>
          <th>Review</th>
        </tr>
      </thead>
      <tbody>
        {#each docs as doc (doc.path)}
          {@const review = openReviewsByPath[doc.path]}
          <tr>
            <td>
              <strong>{doc.title}</strong>
              <div class="design-path">{doc.path}</div>
            </td>
            <td>{doc.status}</td>
            <td>{doc.pending_count}</td>
            <td>{relTime(doc.last_modified)}</td>
            <td>
              {#if review}
                <Link to={`/service/${review.id}`}>
                  In review — {review.status}
                </Link>
              {:else}
                <button class="wb-btn" type="button" onclick={() => openReview(doc)}>
                  Open review Job
                </button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </Section>
{/if}

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
