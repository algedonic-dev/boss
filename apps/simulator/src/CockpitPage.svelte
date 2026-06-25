<script lang="ts">
  // Read-only demo cockpit — a live window into what the brewery
  // tenant is doing right now. No controls here (those live on the
  // Controls page). Two live reads, both through the gateway's public
  // /api surface, exactly as apps/web's landing page does:
  //
  //   GET /api/jobs/live      (poll ~2s) → open_total, per-kind
  //                                        counts, recent jobs, the
  //                                        sim_clock snapshot.
  //   GET /api/events/tail    (poll ~3s) → the most-recent audit_log
  //                                        rows (kind · source · time).
  //
  // apps/web's LandingPage uses /api/jobs/live for the live panel; the
  // event tail mirrors its IT EventsPage (GET /api/events/tail). There
  // is no /api/events/public-tail endpoint, so we use /api/events/tail.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { JobLiveSummary, JobLiveRow, AuditEntry } from './types';

  const LIVE_POLL_MS = 2_000;
  const EVENTS_POLL_MS = 3_000;
  const EVENTS_LIMIT = 40;

  let live = $state<JobLiveSummary | null>(null);
  let liveError = $state<string | null>(null);

  let events = $state<ReadonlyArray<AuditEntry>>([]);
  let eventsError = $state<string | null>(null);

  // Per-kind open counts, sorted by kind for a stable render.
  let countRows = $derived<ReadonlyArray<{ kind: string; count: number }>>(
    live
      ? Object.keys(live.counts)
          .sort()
          .map((k) => ({ kind: k, count: live!.counts[k] ?? 0 }))
      : [],
  );

  let simClock = $derived(live?.sim_clock ?? null);

  async function refreshLive(): Promise<void> {
    try {
      const r = await fetch('/api/jobs/live', { headers: { accept: 'application/json' } });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      live = (await r.json()) as JobLiveSummary;
      liveError = null;
    } catch (e) {
      liveError = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshEvents(): Promise<void> {
    try {
      const r = await fetch(`/api/events/tail?limit=${EVENTS_LIMIT}`, {
        headers: { accept: 'application/json' },
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      events = (await r.json()) as ReadonlyArray<AuditEntry>;
      eventsError = null;
    } catch (e) {
      eventsError = e instanceof Error ? e.message : String(e);
    }
  }

  function fmtTime(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return (
      d.toISOString().slice(0, 10) +
      ' ' +
      String(d.getUTCHours()).padStart(2, '0') +
      ':' +
      String(d.getUTCMinutes()).padStart(2, '0')
    );
  }

  onMount(() => {
    void refreshLive();
    void refreshEvents();
    const liveHandle = window.setInterval(() => void refreshLive(), LIVE_POLL_MS);
    const eventsHandle = window.setInterval(() => void refreshEvents(), EVENTS_POLL_MS);
    return () => {
      window.clearInterval(liveHandle);
      window.clearInterval(eventsHandle);
    };
  });
</script>

<PageHeader
  eyebrow="Simulator"
  title="Demo cockpit"
  subtitle="A live, read-only window into the brewery tenant — jobs in flight and the events landing as the model runs."
  motif="glass"
/>

<div class="cockpit-grid">
  <div class="cockpit-col">
    <Section title="Jobs in flight">
      {#if liveError}
        <p class="status error">Couldn't load live state: {liveError}</p>
      {:else if !live}
        <p class="status">Loading…</p>
      {:else}
        <div class="stat">
          <span class="stat-num">{live.open_total}</span>
          <span class="stat-label">open jobs</span>
        </div>
        {#if countRows.length === 0}
          <p class="status">No open jobs right now.</p>
        {:else}
          <ul class="kind-list">
            {#each countRows as row (row.kind)}
              <li class="kind-row">
                <span class="kind-name">{row.kind}</span>
                <span class="kind-count">{row.count}</span>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </Section>

    <Section title="Simulator clock">
      {#if simClock}
        <dl class="kv">
          <dt>Sim date</dt>
          <dd>{simClock.current_sim_date}</dd>
          <dt>Status</dt>
          <dd>
            <span class="badge" class:paused={simClock.paused}>
              {simClock.paused ? 'Paused' : 'Running'}
            </span>
          </dd>
          {#if simClock.epoch_start_date}
            <dt>Epoch start</dt>
            <dd>{simClock.epoch_start_date}</dd>
          {/if}
          {#if simClock.epoch_end_date}
            <dt>Epoch end</dt>
            <dd>{simClock.epoch_end_date}</dd>
          {/if}
        </dl>
      {:else}
        <p class="status">
          No simulator clock is driving the system — the system time is the wall clock.
        </p>
      {/if}
    </Section>
  </div>

  <div class="cockpit-col">
    <Section title="Recent jobs" wide>
      {#if liveError}
        <p class="status error">Couldn't load recent jobs: {liveError}</p>
      {:else if !live}
        <p class="status">Loading…</p>
      {:else if live.recent.length === 0}
        <p class="status">No recent jobs.</p>
      {:else}
        <ul class="job-list">
          {#each live.recent as job (job.id)}
            {@const j = job as JobLiveRow}
            <li class="job-row">
              <span class="job-kind">{j.kind}</span>
              <span class="job-title">{j.title}</span>
              <span class="job-meta">
                <code>{j.subject_kind}:{j.subject_id}</code> · {j.status} · opened {j.opened_on}
              </span>
            </li>
          {/each}
        </ul>
      {/if}
    </Section>

    <Section title="Live events" wide>
      {#if eventsError}
        <p class="status error">Couldn't load events: {eventsError}</p>
      {:else if events.length === 0}
        <p class="status">No events yet.</p>
      {:else}
        <ul class="event-list">
          {#each events as ev (ev.event_id)}
            <li class="event-row">
              <span class="event-kind">{ev.kind}</span>
              <span class="event-source">{ev.source}</span>
              <span class="event-time">{fmtTime(ev.timestamp)}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </Section>
  </div>
</div>

<style>
  .cockpit-grid {
    display: grid;
    grid-template-columns: minmax(260px, 360px) 1fr;
    gap: 24px;
    align-items: start;
  }
  .cockpit-col {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-width: 0;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 12px;
  }
  .stat-num {
    font-family: var(--font-display);
    font-size: 2.4rem;
    font-weight: 700;
    color: var(--brew-malt-dark);
    line-height: 1;
  }
  .stat-label {
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--brew-malt);
  }
  .kind-list,
  .job-list,
  .event-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .kind-row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .kind-row:hover {
    background: rgba(217, 155, 58, 0.12);
  }
  .kind-name {
    font-style: italic;
    color: var(--brew-malt);
  }
  .kind-count {
    background: var(--brew-malt);
    color: #fff;
    padding: 0.05em 0.55em;
    border-radius: 99px;
    font-size: 0.8rem;
    font-weight: 600;
  }
  .job-list {
    gap: 4px;
    max-height: 360px;
    overflow-y: auto;
  }
  .job-row {
    display: grid;
    grid-template-columns: 9em 1fr;
    grid-template-rows: auto auto;
    gap: 0.1rem 0.6rem;
    padding: 0.45rem 0.6rem;
    border: 1px solid #e6d2a8;
    border-radius: 4px;
    background: var(--brew-cream);
  }
  .job-kind {
    grid-row: 1 / 3;
    align-self: center;
    font-style: italic;
    color: var(--brew-malt);
    font-size: 0.85rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .job-title {
    font-weight: 600;
    line-height: 1.25;
  }
  .job-meta {
    font-size: 0.78rem;
    color: #7a6855;
  }
  .event-list {
    gap: 1px;
    max-height: 360px;
    overflow-y: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
  }
  .event-row {
    display: grid;
    grid-template-columns: 1fr 8em 9.5em;
    gap: 0.5rem;
    padding: 3px 8px;
    border-bottom: 1px solid #f0e6cf;
  }
  .event-kind {
    color: var(--brew-malt-dark);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .event-source {
    color: #7a6855;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .event-time {
    color: #a8a29e;
    text-align: right;
  }
  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    margin: 0;
    font-size: 0.9rem;
  }
  .kv dt {
    color: #78716c;
  }
  .kv dd {
    margin: 0;
    font-weight: 500;
  }
  .badge {
    display: inline-block;
    background: #dcfce7;
    color: #166534;
    border: 1px solid #86efac;
    border-radius: 4px;
    padding: 1px 8px;
    font-size: 0.78rem;
    font-weight: 600;
  }
  .badge.paused {
    background: #fee2e2;
    color: #991b1b;
    border-color: #fca5a5;
  }
  .status {
    margin: 0;
    color: #7a6855;
    font-style: italic;
  }
  .status.error {
    color: #8b2b1f;
    font-style: normal;
  }
  @media (max-width: 820px) {
    .cockpit-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
