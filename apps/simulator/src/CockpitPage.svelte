<script lang="ts">
  // Cockpit — oriented around HOW THE SIMULATOR ENGAGES THE PUBLIC API.
  //
  // The simulator drives the company AS the workforce, entirely through
  // the public HTTP API (x-boss-user: automation:sim + x-sim-origin). It
  // touches the system no other way. This page reads the daemon's own
  // telemetry — what it's calling, how much, how fast — plus the
  // audit-log tail (what landed):
  //
  //   GET /simulator/api/telemetry (poll ~2s) → boss-simulator → the
  //       brewery-sim daemon's localhost control server.
  //   GET /api/events/tail         (poll ~3s) → the audit-log tail.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { SimTelemetry, ApiWrites, AuditEntry } from './types';

  const TELEMETRY_POLL_MS = 2_000;
  const EVENTS_POLL_MS = 3_000;
  const EVENTS_LIMIT = 40;

  let tele = $state<SimTelemetry | null>(null);
  let teleError = $state<string | null>(null);

  let events = $state<ReadonlyArray<AuditEntry>>([]);
  let eventsError = $state<string | null>(null);

  // The per-domain API-write engagement points, in display order — every
  // place the sim POSTs to a domain service as a step side-effect.
  const WRITE_POINTS: ReadonlyArray<{ key: keyof ApiWrites; label: string }> = [
    { key: 'jobs', label: 'Jobs opened' },
    { key: 'invoices_created', label: 'Invoices created' },
    { key: 'invoices_updated', label: 'Invoices updated' },
    { key: 'bank_settlements', label: 'Payments settled' },
    { key: 'shipments', label: 'Shipments' },
    { key: 'purchase_orders', label: 'Purchase orders' },
    { key: 'agreements', label: 'Agreements' },
    { key: 'messages', label: 'Messages' },
    { key: 'tax_filings', label: 'Tax filings' },
    { key: 'account_notes', label: 'Account notes' },
    { key: 'asset_events', label: 'Asset events' },
    { key: 'revenue_schedules', label: 'Revenue schedules' },
    { key: 'scheduled_assignments', label: 'Scheduled assignments' },
  ];

  let writeRows = $derived<ReadonlyArray<{ label: string; count: number }>>(
    tele ? WRITE_POINTS.map((p) => ({ label: p.label, count: tele!.api_writes[p.key] ?? 0 })) : [],
  );

  // Recent ticks, newest first.
  let recentRows = $derived(tele ? [...tele.recent].reverse() : []);

  async function refreshTelemetry(): Promise<void> {
    try {
      const r = await fetch('/simulator/api/telemetry', { headers: { accept: 'application/json' } });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      tele = (await r.json()) as SimTelemetry;
      teleError = null;
    } catch (e) {
      teleError = e instanceof Error ? e.message : String(e);
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
    void refreshTelemetry();
    void refreshEvents();
    const t = window.setInterval(() => void refreshTelemetry(), TELEMETRY_POLL_MS);
    const e = window.setInterval(() => void refreshEvents(), EVENTS_POLL_MS);
    return () => {
      window.clearInterval(t);
      window.clearInterval(e);
    };
  });
</script>

<PageHeader
  eyebrow="Simulator · Public-API engagement"
  title="Cockpit"
  subtitle="How the simulator is driving the company — entirely through the public API, as the workforce."
  motif="glass"
/>

{#if teleError}
  <p class="status error">Couldn't reach the simulator daemon: {teleError}</p>
  <p class="status">The daemon exposes its telemetry on a localhost-only control port; this is
    expected if the simulator isn't running (e.g. a non-demo deployment).</p>
{:else if !tele}
  <p class="status">Loading telemetry…</p>
{:else}
  <!-- Identity + cadence: who the sim is on the API, and at what speed. -->
  <div class="identity">
    <div class="identity-main">
      <span class="id-label">Acting as</span>
      <code class="id-actor">{tele.actor}</code>
      <span class="id-role">{tele.role}</span>
      <span class="id-sep">·</span>
      <span class="id-via">via the public API</span>
      <code class="id-base">{tele.api_base}</code>
    </div>
    <div class="cadence">
      {#if tele.cadence.sim_date}<span class="cad"><b>{tele.cadence.sim_date}</b> sim date</span>{/if}
      {#if tele.cadence.warp_factor}<span class="cad">warp ×{tele.cadence.warp_factor}</span>{/if}
      {#if tele.cadence.days_per_tick && tele.cadence.tick_interval_seconds}
        <span class="cad">{tele.cadence.days_per_tick}d / {tele.cadence.tick_interval_seconds}s per tick</span>
      {/if}
      <span class="badge" class:paused={tele.cadence.paused}>
        {tele.cadence.paused ? 'Paused' : 'Running'}
      </span>
      <span class="cad muted">tick #{tele.tick_count.toLocaleString()}</span>
    </div>
  </div>

  <div class="cockpit-grid">
    <div class="cockpit-col">
      <Section title="Workforce execution">
        <p class="point-sub">
          <code>PUT /api/jobs/&#123;id&#125;/steps</code> — claim &amp; complete assigned work
        </p>
        <div class="stat">
          <span class="stat-num">{tele.workforce.completed.toLocaleString()}</span>
          <span class="stat-label">steps completed</span>
        </div>
        <dl class="kv">
          <dt>Claimed</dt>
          <dd>{tele.workforce.claimed.toLocaleString()}</dd>
          <dt>Deferred (short stock)</dt>
          <dd>{tele.workforce.deferred.toLocaleString()}</dd>
          <dt>Check-ins</dt>
          <dd>{tele.workforce.checkins.toLocaleString()}</dd>
          <dt class:err={tele.workforce.errors > 0}>Errors</dt>
          <dd class:err={tele.workforce.errors > 0}>{tele.workforce.errors.toLocaleString()}</dd>
        </dl>
      </Section>

      <Section title="Recent ticks">
        <p class="point-sub">Per-tick engagement (newest first)</p>
        {#if recentRows.length === 0}
          <p class="status">No ticks yet.</p>
        {:else}
          <ul class="tick-list">
            {#each recentRows as t (t.tick)}
              <li class="tick-row">
                <span class="tick-date">{t.sim_date ?? '—'}</span>
                <span class="tick-deltas">
                  <span class="d done">+{t.completed} done</span>
                  <span class="d claim">+{t.claimed} claimed</span>
                  {#if t.deferred > 0}<span class="d defer">{t.deferred} deferred</span>{/if}
                  {#if t.errors > 0}<span class="d err">{t.errors} err</span>{/if}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </Section>
    </div>

    <div class="cockpit-col">
      <Section title="API writes by domain" wide>
        <p class="point-sub">step.done side-effects → POSTs to the domain services (cumulative)</p>
        <ul class="write-list">
          {#each writeRows as row (row.label)}
            <li class="write-row">
              <span class="write-label">{row.label}</span>
              <span class="write-count">{row.count.toLocaleString()}</span>
            </li>
          {/each}
        </ul>
      </Section>

      <Section title="Live events landing" wide>
        <p class="point-sub">The audit log as the sim's calls land (GET /api/events/tail)</p>
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
{/if}

<style>
  .identity {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    margin-bottom: 20px;
    background: var(--brew-cream);
    border: 1px solid #e6d2a8;
    border-radius: 8px;
  }
  .identity-main {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 0.9rem;
  }
  .id-label {
    color: #7a6855;
  }
  .id-actor {
    font-weight: 700;
    color: var(--brew-malt-dark);
    background: rgba(217, 155, 58, 0.16);
    padding: 0.05em 0.4em;
    border-radius: 4px;
  }
  .id-role {
    font-size: 0.78rem;
    color: var(--brew-malt);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .id-sep {
    color: #c9b896;
  }
  .id-via {
    color: #7a6855;
  }
  .id-base {
    color: #7a6855;
    font-size: 0.82rem;
  }
  .cadence {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 12px;
    font-size: 0.85rem;
    color: var(--brew-malt);
  }
  .cad b {
    color: var(--brew-malt-dark);
  }
  .cad.muted {
    color: #a8a29e;
  }
  .cockpit-grid {
    display: grid;
    grid-template-columns: minmax(280px, 380px) 1fr;
    gap: 24px;
    align-items: start;
  }
  .cockpit-col {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-width: 0;
  }
  .point-sub {
    margin: 0 0 10px;
    font-size: 0.78rem;
    color: #7a6855;
  }
  .point-sub code {
    font-size: 0.74rem;
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
  .kv {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 4px 16px;
    margin: 0;
    font-size: 0.9rem;
  }
  .kv dt {
    color: #78716c;
  }
  .kv dd {
    margin: 0;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .kv dt.err,
  .kv dd.err {
    color: #8b2b1f;
  }
  .write-list,
  .tick-list,
  .event-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .write-row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    padding: 5px 8px;
    border-radius: 4px;
  }
  .write-row:nth-child(odd) {
    background: rgba(217, 155, 58, 0.07);
  }
  .write-label {
    color: var(--brew-malt);
  }
  .write-count {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--brew-malt-dark);
  }
  .tick-list {
    gap: 2px;
    max-height: 300px;
    overflow-y: auto;
  }
  .tick-row {
    display: grid;
    grid-template-columns: 6.5em 1fr;
    gap: 0.5rem;
    align-items: baseline;
    padding: 3px 8px;
    border-bottom: 1px solid #f0e6cf;
    font-size: 0.82rem;
  }
  .tick-date {
    color: #7a6855;
    font-variant-numeric: tabular-nums;
  }
  .tick-deltas {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .d {
    font-variant-numeric: tabular-nums;
  }
  .d.done {
    color: #166534;
    font-weight: 600;
  }
  .d.claim {
    color: var(--brew-malt);
  }
  .d.defer {
    color: #92400e;
  }
  .d.err {
    color: #8b2b1f;
    font-weight: 600;
  }
  .event-list {
    gap: 1px;
    max-height: 300px;
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
    margin: 0 0 6px;
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
