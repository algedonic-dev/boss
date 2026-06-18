<script lang="ts">
  // Floating top-right "Simulator" panel — sim clock display +
  // operator-facing controls for triggering sim events. Replaces the
  // earlier bottom-right SimClockBadge (display only) so the
  // sim-control buttons (Reset, New shop order, Wholesale order,
  // Anomaly) sit alongside the clock they affect, and the bottom
  // corners free up for the role-gated DebugGear (lower-right).
  //
  // Subscribes to /api/jobs/sim-clock/stream (SSE) and renders the
  // current sim-day as the brewery clock advances in real time
  // (1 sim-day every 10 wall-seconds at default cadence).
  //
  // 12-month epoch + reset-to-baseline: the Reset
  // button (always visible) POSTs /api/jobs/sim-clock/restart-epoch
  // which truncates audit_log, re-imports the canonical seed, replays
  // projections, resets the clock, and unpauses. ~30-60s; the button
  // shows "Restarting…" while it runs. The contextual Restart-epoch
  // CTA stays for the epoch-complete case (older muscle memory + the
  // demo audience signal).

  import type { SimClockState } from '../landing/types';
  import { toggleSimPause, type SimLogger } from '../debug/simulations';
  import { simClock } from './sim-clock.svelte';
  import { session } from '../session/session.svelte';

  // The clock state itself lives in the shared `simClock` rune so
  // every page can read it via `appToday()` / `appNow()`. This
  // component owns the network plumbing (SSE + poll fallback) plus
  // the operator UI; the rune-backed store is the read interface
  // for the rest of the SPA.
  let clock = $derived<SimClockState | null>(simClock.value);
  let restartError = $state<string | null>(null);
  let restartStartedAt = $state<number | null>(null);
  let nowMs = $state<number>(Date.now());

  type LogEntry = { at: string; msg: string; level: 'info' | 'error' };
  let log = $state<LogEntry[]>([]);
  let running = $state(false);
  const LOG_MAX = 12;

  function applyClockUpdate(next: SimClockState | null): void {
    if (clock?.restart_in_progress && next?.restart_in_progress === false) {
      restartStartedAt = null;
    }
    simClock.set(next);
  }

  async function fallbackPoll(): Promise<void> {
    try {
      const r = await fetch('/api/jobs/live');
      if (!r.ok) return;
      const body = (await r.json()) as { sim_clock?: SimClockState | null };
      applyClockUpdate(body.sim_clock ?? null);
    } catch {
      // Silent — SSE will deliver state once it connects.
    }
  }

  async function restartEpoch(): Promise<void> {
    if (clock?.restart_in_progress) return;
    if (
      !confirm(
        'Reset the simulator? Trims all audit_log events after the epoch baseline and rewinds the sim clock to epoch_start. The brewery returns to day 1 of the 12-month loop.',
      )
    ) {
      return;
    }
    restartError = null;
    restartStartedAt = Date.now();
    try {
      const r = await fetch('/api/jobs/sim-clock/restart-epoch', { method: 'POST' });
      if (!r.ok) {
        const text = await r.text().catch(() => '');
        restartError = `HTTP ${r.status}${text ? `: ${text.slice(0, 120)}` : ''}`;
        restartStartedAt = null;
      } else {
        simClock.set((await r.json()) as SimClockState | null);
      }
    } catch (e) {
      restartError = e instanceof Error ? e.message : String(e);
      restartStartedAt = null;
    }
  }

  function hhmmss(): string {
    const d = new Date();
    return [d.getHours(), d.getMinutes(), d.getSeconds()]
      .map((n) => n.toString().padStart(2, '0'))
      .join(':');
  }

  function appendLog(msg: string, level: 'info' | 'error' = 'info'): void {
    log = [...log, { at: hhmmss(), msg, level }].slice(-LOG_MAX);
  }

  async function runSim(label: string, fn: (l: SimLogger) => Promise<void>): Promise<void> {
    if (running) return;
    running = true;
    appendLog(`— ${label} —`);
    try {
      await fn(appendLog);
    } catch (e) {
      appendLog(e instanceof Error ? e.message : String(e), 'error');
    } finally {
      running = false;
    }
  }

  $effect(() => {
    void fallbackPoll();
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;

    try {
      es = new EventSource('/api/jobs/sim-clock/stream');
      es.onmessage = (ev) => {
        try {
          applyClockUpdate(JSON.parse(ev.data) as SimClockState);
        } catch {
          // Malformed frame — drop and wait for the next one.
        }
      };
      es.onerror = () => {
        if (es && es.readyState === EventSource.CLOSED) {
          es.close();
          es = null;
          if (pollFallbackId === null) {
            pollFallbackId = window.setInterval(fallbackPoll, 20_000);
          }
        }
      };
    } catch {
      pollFallbackId = window.setInterval(fallbackPoll, 20_000);
    }

    return () => {
      es?.close();
      if (pollFallbackId !== null) window.clearInterval(pollFallbackId);
    };
  });

  $effect(() => {
    if (!clock?.restart_in_progress) return;
    nowMs = Date.now();
    const id = window.setInterval(() => {
      nowMs = Date.now();
    }, 1_000);
    return () => window.clearInterval(id);
  });

  let restartElapsedSec = $derived.by(() => {
    if (!restartStartedAt) return null;
    return Math.max(0, Math.floor((nowMs - restartStartedAt) / 1000));
  });

  let epochComplete = $derived.by(() => {
    if (!clock) return false;
    return (
      clock.paused &&
      !!clock.epoch_end_date &&
      clock.current_sim_date >= clock.epoch_end_date
    );
  });

  // Format the sim-time as "YYYY-MM-DD HH:MM" so the within-day
  // movement of the formula clock is visible. Falls back to the
  // date-only field if `now` isn't populated (older backends).
  function fmtSimNow(c: SimClockState): string {
    const iso = c.now ?? c.current_sim_date;
    if (!iso) return '';
    if (iso.length === 10) return iso; // date-only fallback
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    const date = d.toISOString().slice(0, 10);
    const hh = String(d.getUTCHours()).padStart(2, '0');
    const mm = String(d.getUTCMinutes()).padStart(2, '0');
    return `${date} ${hh}:${mm}`;
  }

  let label = $derived.by(() => {
    if (!clock) return '';
    if (clock.restart_in_progress) {
      const elapsed = restartElapsedSec;
      const t = elapsed === null ? '' : ` (${Math.floor(elapsed / 60)}m ${elapsed % 60}s)`;
      return `Restarting…${t}`;
    }
    const stamp = fmtSimNow(clock);
    if (epochComplete) {
      return `Epoch complete — ${stamp}`;
    }
    if (clock.paused) {
      return `Paused — ${stamp}`;
    }
    return stamp;
  });

  let resetDisabled = $derived(running || clock?.restart_in_progress === true);
  let triggerDisabled = $derived(running || clock?.restart_in_progress === true);

  // Sim controls (Pause/Reset) mutate the shared clock + trim audit_log,
  // so they're for real logged-in operators only — not demo/anonymous
  // visitors (who get an audit-readonly gateway session) or client-side
  // persona views (`fromGateway === false`, a demo affordance). Audit-only
  // users see the read-only "System Sim Time" display with no buttons.
  let canControlSim = $derived(
    session.fromGateway &&
      session.value.kind === 'ready' &&
      session.value.user.role !== 'audit-readonly',
  );
</script>

{#if clock}
  <div
    class="sim-panel"
    class:sim-panel-paused={clock.paused}
    title={`Sim epoch ${clock.epoch_start_date ?? '?'} → ${clock.epoch_end_date ?? '?'} · 1 sim-day per 10 wall-seconds`}
  >
    <div class="sim-panel-eyebrow">{canControlSim ? 'Simulator' : 'System Sim Time'}</div>
    <div class="sim-panel-date">{label}</div>

    {#if canControlSim}
      <div class="sim-panel-actions">
        <button
          type="button"
          class="sim-panel-btn"
          disabled={triggerDisabled}
          onclick={() => runSim(clock.paused ? 'Resume sim' : 'Pause sim', toggleSimPause)}
          title={clock.paused ? 'Resume the sim clock' : 'Pause the sim clock'}
        >
          {clock.paused ? 'Play ▶︎' : 'Pause ❚❚'}
        </button>
        <button
          type="button"
          class="sim-panel-btn sim-panel-btn-reset"
          disabled={resetDisabled}
          onclick={restartEpoch}
          title="Trim audit_log past the epoch baseline + rewind sim_clock to epoch_start"
        >
          Reset ↻
        </button>
      </div>

      {#if epochComplete && !clock.restart_in_progress}
        <div class="sim-panel-cta">Epoch complete — Reset above to roll back to year 1.</div>
      {/if}
      {#if restartError}
        <div class="sim-panel-err">{restartError}</div>
      {/if}

      {#if log.length > 0}
        <div class="sim-panel-log">
          {#each log as entry, i (i)}
            <div class="sim-panel-log-row" class:sim-panel-log-err={entry.level === 'error'}>
              <span class="sim-panel-log-at">{entry.at}</span>
              <span class="sim-panel-log-msg">{entry.msg}</span>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .sim-panel {
    position: fixed;
    top: 16px;
    right: 16px;
    z-index: 50;
    background: rgba(28, 25, 23, 0.92);
    color: #fef3c7;
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 12px;
    line-height: 1.3;
    min-width: 220px;
    max-width: 280px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18);
    backdrop-filter: blur(4px);
  }
  .sim-panel-paused {
    background: rgba(120, 53, 15, 0.92);
  }
  .sim-panel-eyebrow {
    font-size: 10px;
    letter-spacing: 0.6px;
    text-transform: uppercase;
    color: rgba(254, 243, 199, 0.7);
    font-weight: 500;
  }
  .sim-panel-date {
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    margin-top: 2px;
  }
  .sim-panel-actions {
    margin-top: 10px;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .sim-panel-btn {
    padding: 6px 8px;
    font-size: 11px;
    font-weight: 500;
    background: #44403c;
    color: #fef3c7;
    border: 1px solid #57534e;
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.12s ease;
    text-align: center;
  }
  .sim-panel-btn:hover:not(:disabled) {
    background: #57534e;
  }
  .sim-panel-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .sim-panel-btn-reset {
    background: #fbbf24;
    color: #1c1917;
    border-color: #f59e0b;
    grid-column: 1 / -1;
  }
  .sim-panel-btn-reset:hover:not(:disabled) {
    background: #f59e0b;
  }
  .sim-panel-cta {
    margin-top: 8px;
    font-size: 11px;
    color: rgba(254, 243, 199, 0.85);
    font-style: italic;
  }
  .sim-panel-err {
    margin-top: 6px;
    font-size: 11px;
    color: #fca5a5;
  }
  .sim-panel-log {
    margin-top: 8px;
    padding-top: 8px;
    border-top: 1px solid rgba(254, 243, 199, 0.15);
    font-size: 10px;
    font-family: ui-monospace, monospace;
    max-height: 140px;
    overflow-y: auto;
    color: rgba(254, 243, 199, 0.85);
  }
  .sim-panel-log-row {
    display: flex;
    gap: 6px;
    line-height: 1.4;
  }
  .sim-panel-log-at {
    color: rgba(254, 243, 199, 0.5);
    flex-shrink: 0;
  }
  .sim-panel-log-msg {
    word-break: break-word;
  }
  .sim-panel-log-err .sim-panel-log-msg {
    color: #fca5a5;
  }
</style>
