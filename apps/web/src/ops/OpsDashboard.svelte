<script lang="ts">
  // Cybernetics observability — port of apps/web/src/OpsDashboard.tsx.
  //
  // Polls /api/snapshot on a 5s interval; subscribes to /api/events
  // via EventSource for live telemetry. Four panels: StackHealth,
  // DispatchStatus, QueueDepths, MessageFlow.

  import { onMount } from 'svelte';
  import type {
    AgentSpec,
    HealthBody,
    QueueEntry,
    RunHandle,
    Snapshot,
    TelemetryEvent,
    VmResult,
  } from './types';
  import { TELEMETRY_KINDS } from './types';

  type SnapState =
    | { kind: 'loading' }
    | { kind: 'ready'; data: Snapshot; fetchedAt: Date }
    | { kind: 'error'; error: string };

  let snapState: SnapState = $state<SnapState>({ kind: 'loading' });
  let events: TelemetryEvent[] = $state([]);
  let connection: 'connecting' | 'open' | 'closed' = $state('connecting');

  const INTERVAL_MS = 5000;
  const EVENT_BUFFER = 200;

  onMount(() => {
    let cancelled = false;
    const load = async (): Promise<void> => {
      try {
        const resp = await fetch('/api/snapshot');
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const data = (await resp.json()) as Snapshot;
        if (!cancelled) snapState = { kind: 'ready', data, fetchedAt: new Date() };
      } catch (e) {
        if (!cancelled) snapState = { kind: 'error', error: String(e) };
      }
    };
    void load();
    const timer = setInterval(() => void load(), INTERVAL_MS);

    const es = new EventSource('/api/events');
    connection = 'connecting';
    es.onopen = () => (connection = 'open');
    es.onerror = () => (connection = 'closed');

    const onEvent = (e: MessageEvent<string>): void => {
      try {
        const ev = JSON.parse(e.data) as TelemetryEvent;
        events = [ev, ...events].slice(0, EVENT_BUFFER);
      } catch {
        // ignore malformed
      }
    };
    for (const k of TELEMETRY_KINDS) {
      es.addEventListener(k, onEvent as EventListener);
    }

    return () => {
      cancelled = true;
      clearInterval(timer);
      for (const k of TELEMETRY_KINDS) {
        es.removeEventListener(k, onEvent as EventListener);
      }
      es.close();
    };
  });

  // ------------------------------------------------------------
  // Derived rows
  // ------------------------------------------------------------

  let dispatchRows = $derived.by(() => {
    if (snapState.kind !== 'ready') return [];
    const agents = snapState.data.agents;
    const runs = snapState.data.runs;
    return agents.flatMap((specs) => {
      if (specs.error || !Array.isArray(specs.body)) return [];
      const vmRuns =
        runs.find((r) => r.vm_id === specs.vm_id && Array.isArray(r.body))?.body ??
        [];
      return specs.body.map((a: AgentSpec) => ({
        vm: specs.vm_id,
        agent: a.id,
        running: (vmRuns as RunHandle[]).filter((r) => r.agent === a.id).length,
        cap: a.max_concurrent_runs,
      }));
    });
  });

  let queueRows = $derived.by(() => {
    if (snapState.kind !== 'ready') return [];
    return snapState.data.queues.flatMap((v) =>
      v.error || !Array.isArray(v.body)
        ? []
        : (v.body as QueueEntry[]).map((q) => ({
            vm: v.vm_id,
            agent: q.agent,
            depth: q.depth,
          })),
    );
  });

  function healthDot(h: VmResult<HealthBody>): string {
    if (h.error) return 'err';
    if (h.status !== 200) return 'warn';
    return 'ok';
  }

  function trailingToken(kind: string): string {
    const parts = kind.split('.');
    return parts[parts.length - 1] ?? '';
  }

  function formatTs(iso: string): string {
    const d = new Date(iso);
    return d.toLocaleTimeString(undefined, { hour12: false });
  }
</script>

<div class="app theme-ops">
  <header class="app-header">
    <h1>BOSS — Cybernetics Observability</h1>
    <div class="status">
      {#if snapState.kind === 'ready'}
        snapshot @ {snapState.fetchedAt.toLocaleTimeString()}
      {:else if snapState.kind === 'loading'}
        loading…
      {:else}
        <span class="error">{snapState.error}</span>
      {/if}
    </div>
  </header>

  {#if snapState.kind === 'ready' && snapState.data.demo_mode === true}
    <div class="ops-demo-banner" role="status">
      <strong>Synthetic agent telemetry — no real LLM workers in this deployment.</strong>
      The agent specs, queue depths, in-flight runs, and token-spend
      figures below come from <code>infra/observability/config.toml</code>'s
      <code>[demo_agents]</code> block (see <code>boss-observability/src/demo_agents.rs</code>).
      The Message Flow panel's events are pushed by the same synthetic
      loop. Attach real workers by populating <code>[[vms]]</code> against
      a <code>boss-cybernetics</code> deployment and removing
      <code>[demo_agents]</code>.
    </div>
  {/if}

  <div class="grid">
    <section class="panel">
      <h2>Stack Health</h2>
      {#if snapState.kind === 'ready'}
        {@const health = snapState.data.health}
        {#if health.length === 0}
          <p class="empty">No VMs configured.</p>
        {:else}
          {@const up = health.filter((h) => h.status === 200 && !h.error).length}
          <div class="status">
            <span class="dot ok"></span>
            {up} / {health.length} VMs up
          </div>
          <table>
            <thead>
              <tr>
                <th>VM</th>
                <th>Status</th>
                <th>Last report</th>
              </tr>
            </thead>
            <tbody>
              {#each health as h (h.vm_id)}
                <tr>
                  <td>{h.vm_id}</td>
                  <td>
                    <span class="dot {healthDot(h)}"></span>
                    {h.error ?? `${h.status} ${h.body?.status ?? ''}`}
                  </td>
                  <td>{h.body?.timestamp ?? '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {:else}
        <p class="empty">—</p>
      {/if}
    </section>

    <section class="panel">
      <h2>Dispatch Status</h2>
      {#if snapState.kind === 'ready'}
        {#if dispatchRows.length === 0}
          <p class="empty">No agents registered.</p>
        {:else}
          <table>
            <thead>
              <tr>
                <th>VM</th>
                <th>Agent</th>
                <th style="text-align:right">Running / Cap</th>
              </tr>
            </thead>
            <tbody>
              {#each dispatchRows as r (`${r.vm}/${r.agent}`)}
                <tr>
                  <td>{r.vm}</td>
                  <td>{r.agent}</td>
                  <td style="text-align:right">
                    <span class="dot {r.running >= r.cap ? 'warn' : 'ok'}"></span>
                    {r.running} / {r.cap}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {:else}
        <p class="empty">—</p>
      {/if}
    </section>

    <section class="panel">
      <h2>Queue Depths</h2>
      {#if snapState.kind === 'ready'}
        {#if queueRows.length === 0}
          <p class="empty">No queues.</p>
        {:else}
          <table>
            <thead>
              <tr>
                <th>VM</th>
                <th>Agent</th>
                <th style="text-align:right">Depth</th>
              </tr>
            </thead>
            <tbody>
              {#each queueRows as r (`${r.vm}/${r.agent}`)}
                <tr>
                  <td>{r.vm}</td>
                  <td>{r.agent}</td>
                  <td style="text-align:right">{r.depth}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {:else}
        <p class="empty">—</p>
      {/if}
    </section>

    <section class="panel full">
      <h2>Message Flow</h2>
      <div class="status" style="margin-bottom:8px">
        <span class="dot {connection === 'open' ? 'ok' : connection === 'closed' ? 'err' : 'warn'}"></span>
        SSE {connection}
        <span style="margin-left:12px">{events.length} recent</span>
      </div>
      {#if events.length === 0}
        <p class="empty">Waiting for events…</p>
      {:else}
        <div class="event-stream">
          {#each events as e (e.id)}
            <div class="event-row {trailingToken(e.kind)}">
              <span class="event-ts">{formatTs(e.timestamp)}</span>
              <span class="event-kind">{e.kind}</span>
              <span class="event-source">{e.source}</span>
            </div>
          {/each}
        </div>
      {/if}
    </section>
  </div>
</div>

<style>
  .ops-demo-banner {
    padding: 12px 14px;
    background: #fff7ed;
    border: 1px solid #fdba74;
    border-radius: 6px;
    font-size: 13px;
    color: #7c2d12;
    margin: 12px 16px;
    line-height: 1.5;
  }
  .ops-demo-banner strong { display: block; margin-bottom: 4px; }
  .ops-demo-banner code {
    font-size: 12px;
    background: #fef3c7;
    padding: 1px 4px;
    border-radius: 3px;
  }
</style>
