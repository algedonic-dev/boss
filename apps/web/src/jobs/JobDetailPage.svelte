<script lang="ts">
  // Job Detail — the work surface for one Job.
  //
  // Hero + subject panel + per-step work surface via StepSurface
  // (which dispatches to the approval surface, the generic surface,
  // or a React plugin bundle based on step.kind).

  import { navigate, href } from '../router';
  import { shortId } from '../data/ids';
  import {
    subjectLabel,
    subjectPath,
    type Job,
  } from './types';
  import PageHeader from '../ui/PageHeader.svelte';
  import Section from '../ui/Section.svelte';
  import StepSurface from '../steps/StepSurface.svelte';
  import StepGraph from './StepGraph.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';

  let { jobId } = $props<{ jobId: string }>();

  let job = $state<Job | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Two paths:
  // 1. /api/jobs/{id}/stream — SSE that pushes a JobDetail frame
  //    on every observable change (job status / priority / closed_on,
  //    or any step's status / completed_on). Per the SSE policy doc
  //    this view is "state-machine state where a single event flips
  //    the visible value" → SSE-push. Shipped 2026-05-01.
  // 2. fetch /api/jobs/{id} — fallback when SSE fails (older deploys).
  //    Also used by post-action callbacks (StepSurface PUT → onStepUpdate)
  //    to refresh immediately rather than waiting for the next SSE
  //    poll tick on the server.
  async function load() {
    const id = jobId;
    loading = true;
    try {
      const resp = await fetch(`/api/jobs/${encodeURIComponent(id)}`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      job = (await resp.json()) as Job;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const id = jobId;
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;
    let cancelled = false;

    try {
      es = new EventSource(`/api/jobs/${encodeURIComponent(id)}/stream`);
      es.onmessage = (ev) => {
        if (cancelled) return;
        try {
          const detail = JSON.parse(ev.data) as Job;
          job = detail;
          loading = false;
          error = null;
        } catch {
          // Drop malformed frame; next push will fix it.
        }
      };
      es.addEventListener('error', () => {
        if (es && es.readyState === EventSource.CLOSED) {
          es.close();
          es = null;
          // On 404 / proxy down, fall back to the on-mount fetch
          // + slow-poll. On a transient blip the browser
          // auto-reconnects, so we only fall through on CLOSED.
          if (pollFallbackId === null) {
            void load();
            pollFallbackId = window.setInterval(() => void load(), 30_000);
          }
        }
      });
      es.addEventListener('gone', () => {
        // Server says the Job is gone — refetch once so error
        // state lands consistently.
        void load();
      });
    } catch {
      void load();
      pollFallbackId = window.setInterval(() => void load(), 30_000);
    }

    return () => {
      cancelled = true;
      es?.close();
      if (pollFallbackId !== null) window.clearInterval(pollFallbackId);
    };
  });

  function onStepUpdate(): void {
    // Eager refetch after the operator clicks done/sign-off so
    // the page updates without waiting for the next 2s SSE tick.
    void load();
  }
</script>

{#if loading}
  <div class="catalog theme-exec"><p class="empty">Loading…</p></div>
{:else if error || !job}
  <div class="catalog theme-exec">
    <p class="empty">Couldn't load job: {error ?? 'not found'}</p>
  </div>
{:else}
  {@const j = job}
  <div class="catalog theme-exec">
    <PageHeader
      eyebrow={`${j.kind} · ${j.status}`}
      title={j.title}
      subtitle={`Opened ${j.opened_on}${j.due_on ? ` · due ${j.due_on}` : ''} · owner ${j.owner_id}`}
    />

    <div class="tab-grid">
      <Section title="Subject">
          <div class="jd-info-row">
            <span class="jd-info-label">Kind</span>
            <span class="jd-info-value">{j.subject.subject_kind}</span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">ID</span>
            <span class="jd-info-value jd-mono">
              <a
                href={href(subjectPath(j.subject))}
                onclick={(e) => {
                  e.preventDefault();
                  navigate(href(subjectPath(j.subject)));
                }}
              >
                {subjectLabel(j.subject)}
              </a>
            </span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">BOSS Job ID</span>
            <span class="jd-info-value jd-mono">{shortId(j.id)}</span>
          </div>
      </Section>

      <Section title="Attachments">
        <FileAttachments targetKind="job" targetId={j.id} />
      </Section>

      <Section title={`Steps (${j.steps?.length ?? 0})`} wide>
          {#if !j.steps || j.steps.length === 0}
            <p class="empty">No steps on this job yet.</p>
          {:else}
            <StepGraph
              steps={j.steps.map((s) => ({ ...s, notes: s.notes ?? null }))}
              {jobId}
              onUpdate={onStepUpdate}
            />
          {/if}
      </Section>
    </div>
  </div>
{/if}
