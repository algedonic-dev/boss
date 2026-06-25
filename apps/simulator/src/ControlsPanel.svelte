<script lang="ts">
  // Engine controls for the simulator. POSTs to the boss-simulator
  // service's control surface under /simulator/api/control/*:
  //
  //   POST /simulator/api/control/pause          (no body)
  //   POST /simulator/api/control/resume         (no body)
  //   POST /simulator/api/control/restart-epoch  (no body) — confirm()
  //   POST /simulator/api/control/configure      { epoch_start?,
  //                                                epoch_end?,
  //                                                warp_factor? }
  //
  // All control endpoints return the updated SimClockState on success.
  // A 403 means the visitor isn't a signed-in operator — the engine
  // controls are operator-only, so we render a read-only notice and
  // disable the buttons rather than surfacing it as an error.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { SimClockState, JobLiveSummary } from './types';

  const STATUS_POLL_MS = 3_000;

  let clock = $state<SimClockState | null>(null);
  // Set true once any control returns 403 — the visitor can't drive
  // the engine. Latches so the whole panel switches to read-only.
  let readOnly = $state(false);
  // Transient feedback for the last action (success or failure).
  let notice = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  // In-flight guard so double-clicks don't fire a control twice.
  let busy = $state(false);

  // Configure-form fields. All optional — only non-blank fields are
  // sent. warp_factor is parsed to a number; blank means "leave it".
  let epochStart = $state('');
  let epochEnd = $state('');
  let warpFactor = $state('');

  let paused = $derived(clock?.paused ?? false);

  // Poll the live sim_clock so `paused` (and the rest of the clock)
  // stays current even when another operator drives the engine.
  async function refreshClock(): Promise<void> {
    try {
      const r = await fetch('/api/jobs/live', { headers: { accept: 'application/json' } });
      if (!r.ok) return;
      const body = (await r.json()) as JobLiveSummary;
      clock = body.sim_clock ?? null;
    } catch {
      // Silent — the next poll retries.
    }
  }

  // Shared POST helper. On 200, swallows the updated SimClockState and
  // refreshes local state. On 403, latches read-only mode. Otherwise
  // surfaces the error text.
  async function postControl(
    path: string,
    body?: Record<string, unknown>,
  ): Promise<boolean> {
    busy = true;
    actionError = null;
    notice = null;
    try {
      const r = await fetch(`/simulator/api/control/${path}`, {
        method: 'POST',
        headers: body
          ? { 'content-type': 'application/json', accept: 'application/json' }
          : { accept: 'application/json' },
        body: body ? JSON.stringify(body) : undefined,
      });
      if (r.status === 403) {
        readOnly = true;
        return false;
      }
      if (!r.ok) {
        const msg = await r.text();
        actionError = `HTTP ${r.status}${msg ? `: ${msg}` : ''}`;
        return false;
      }
      // Success — the endpoint returns the updated SimClockState.
      try {
        clock = (await r.json()) as SimClockState;
      } catch {
        // No/!JSON body — fall back to a fresh poll.
        await refreshClock();
      }
      return true;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function togglePause(): Promise<void> {
    const path = paused ? 'resume' : 'pause';
    if (await postControl(path)) {
      notice = paused ? 'Resumed.' : 'Paused.';
    }
  }

  async function restartEpoch(): Promise<void> {
    if (
      !window.confirm(
        'Restart the epoch? This rewinds the simulator clock and replays the model from the start of the epoch.',
      )
    ) {
      return;
    }
    if (await postControl('restart-epoch')) {
      notice = 'Epoch restart requested.';
    }
  }

  async function submitConfigure(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const body: Record<string, unknown> = {};
    if (epochStart.trim()) body['epoch_start'] = epochStart.trim();
    if (epochEnd.trim()) body['epoch_end'] = epochEnd.trim();
    if (warpFactor.trim()) {
      const n = Number(warpFactor.trim());
      if (Number.isFinite(n)) body['warp_factor'] = n;
    }
    if (Object.keys(body).length === 0) {
      actionError = 'Nothing to configure — set at least one field.';
      return;
    }
    if (await postControl('configure', body)) {
      notice = 'Configuration applied.';
    }
  }

  onMount(() => {
    void refreshClock();
    const handle = window.setInterval(() => void refreshClock(), STATUS_POLL_MS);
    return () => window.clearInterval(handle);
  });
</script>

<PageHeader
  eyebrow="Simulator"
  title="Engine controls"
  subtitle="Pause, resume, restart, and configure the simulator that drives the brewery tenant."
  motif="tap"
/>

{#if readOnly}
  <div class="notice readonly" role="status">
    <strong>Sim controls require a signed-in operator.</strong>
    You're viewing read-only — the controls below are disabled. Sign in
    as an operator to drive the simulator engine.
  </div>
{/if}

{#if notice}
  <div class="notice ok" role="status">{notice}</div>
{/if}
{#if actionError}
  <div class="notice err" role="alert">{actionError}</div>
{/if}

<div class="controls-grid">
  <Section title="Run state">
    <p class="run-state">
      The simulator is currently
      <span class="badge" class:paused>{paused ? 'paused' : 'running'}</span>.
    </p>
    <div class="btn-row">
      <button
        type="button"
        class="btn primary"
        disabled={readOnly || busy}
        onclick={togglePause}
      >
        {paused ? 'Resume' : 'Pause'}
      </button>
      <button
        type="button"
        class="btn danger"
        disabled={readOnly || busy}
        onclick={restartEpoch}
      >
        Restart epoch
      </button>
    </div>
  </Section>

  <Section title="Configure">
    <form class="configure-form" onsubmit={submitConfigure}>
      <label class="field">
        <span class="field-label">Epoch start (YYYY-MM-DD)</span>
        <input
          type="date"
          bind:value={epochStart}
          disabled={readOnly || busy}
          placeholder="YYYY-MM-DD"
        />
      </label>
      <label class="field">
        <span class="field-label">Epoch end (YYYY-MM-DD)</span>
        <input
          type="date"
          bind:value={epochEnd}
          disabled={readOnly || busy}
          placeholder="YYYY-MM-DD"
        />
      </label>
      <label class="field">
        <span class="field-label">Warp factor</span>
        <input
          type="number"
          min="1"
          step="1"
          bind:value={warpFactor}
          disabled={readOnly || busy}
          placeholder="e.g. 1000"
        />
      </label>
      <div class="btn-row">
        <button type="submit" class="btn primary" disabled={readOnly || busy}>
          Apply configuration
        </button>
      </div>
      <p class="hint">All fields optional — only the fields you set are changed.</p>
    </form>
  </Section>
</div>

<style>
  .controls-grid {
    display: grid;
    grid-template-columns: minmax(280px, 1fr) minmax(280px, 1fr);
    gap: 24px;
    align-items: start;
  }
  .notice {
    border-radius: 6px;
    padding: 12px 16px;
    margin-bottom: 16px;
    font-size: 0.9rem;
    line-height: 1.5;
  }
  .notice.readonly {
    background: #fef3c7;
    border: 1px solid #fcd34d;
    color: #92400e;
  }
  .notice.ok {
    background: #dcfce7;
    border: 1px solid #86efac;
    color: #166534;
  }
  .notice.err {
    background: #fee2e2;
    border: 1px solid #fca5a5;
    color: #991b1b;
  }
  .run-state {
    margin: 0 0 12px;
    font-size: 0.95rem;
  }
  .badge {
    display: inline-block;
    background: #dcfce7;
    color: #166534;
    border: 1px solid #86efac;
    border-radius: 4px;
    padding: 1px 8px;
    font-size: 0.8rem;
    font-weight: 600;
  }
  .badge.paused {
    background: #fee2e2;
    color: #991b1b;
    border-color: #fca5a5;
  }
  .btn-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .btn {
    padding: 8px 16px;
    border-radius: 6px;
    border: 1px solid var(--brew-amber, #d99b3a);
    background: #fff;
    color: var(--brew-malt, #7a3f1f);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    transition: background 80ms, opacity 80ms;
  }
  .btn:hover:not(:disabled) {
    background: var(--brew-amber-bg, #fff7e0);
  }
  .btn.primary {
    background: var(--brew-amber, #d99b3a);
    color: #fff;
  }
  .btn.primary:hover:not(:disabled) {
    background: #c2862c;
  }
  .btn.danger {
    border-color: #fca5a5;
    color: #991b1b;
  }
  .btn.danger:hover:not(:disabled) {
    background: #fee2e2;
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .configure-form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .field-label {
    font-size: 0.8rem;
    color: #78716c;
    font-weight: 500;
  }
  .field input {
    padding: 6px 10px;
    border: 1px solid #d6d3d1;
    border-radius: 6px;
    font: inherit;
    background: #fafaf9;
  }
  .field input:disabled {
    opacity: 0.6;
  }
  .hint {
    margin: 0;
    font-size: 0.8rem;
    color: #a8a29e;
  }
  @media (max-width: 820px) {
    .controls-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
