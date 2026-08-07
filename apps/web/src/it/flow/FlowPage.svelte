<script lang="ts">
  // /it/flow — what the IT team is getting through.
  //
  // The other IT instruments report the machine: what the system is
  // doing under load. This reports the team — what was filed, what got
  // routed, what closed, and how long a person waited for an answer.
  //
  // ## The clock this page runs on
  //
  // Wall clock, and that is the whole reason `/api/views/flow` exists
  // rather than this reading `/api/jobs`. Event time in BOSS is
  // clock-authoritative, and on a demo deployment the authoritative
  // clock is the simulator's — so a real person's feedback Job carries
  // an `opened_on` in the simulated brewery's calendar. The epoch runs
  // 366 sim-days in about nine real hours, so a ten-minute triage
  // reads as a week on those stamps. Only `audit_log.created_at` keeps
  // the real time, and the view reads it there.
  //
  // ## Whose work counts
  //
  // JobKinds that declare an owner_role the server was asked for —
  // registry data, never a list here. That is also what keeps the 85
  // mis-marked restock Jobs out: they declare no owner, so they never
  // enter, where a plain "not simulated" filter would have folded a
  // brewery's restocking into the IT team's numbers.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { type Fork, readFork, forkStep, disposition } from '../../jobs/fork';
  import type { Job } from '../../jobs/types';

  type FlowStep = Readonly<{
    step_id: string;
    status: string;
    metadata: Record<string, unknown>;
    field_names: ReadonlyArray<string>;
    last_written_at: string | null;
  }>;
  type FlowJob = Readonly<{
    job_id: string;
    kind: string;
    title: string;
    status: string;
    filed_at: string | null;
    last_activity_at: string | null;
    steps: ReadonlyArray<FlowStep>;
  }>;
  type Flow = Readonly<{
    owner_roles: ReadonlyArray<string>;
    kinds: ReadonlyArray<string>;
    jobs: ReadonlyArray<FlowJob>;
    as_of: string;
  }>;

  const WINDOWS = [
    { id: '1', label: '24 hours', hours: 24 },
    { id: '7', label: '7 days', hours: 24 * 7 },
    { id: 'all', label: 'All time', hours: Number.POSITIVE_INFINITY },
  ] as const;

  let flow = $state<Flow | null>(null);
  let forks = $state<Record<string, Fork | null>>({});
  let windowId = $state<string>('7');
  let loading = $state(true);
  let error = $state<string | null>(null);

  let windowHours = $derived(WINDOWS.find((w) => w.id === windowId)?.hours ?? 168);

  /// The view returns raw steps; the fork rule lives in jobs/fork.ts
  /// and is applied here. Shaping a FlowJob into the `Job` the rule
  /// expects keeps ONE definition of "which step carries the
  /// decision" — it drifted once between the board and the terminal
  /// reader, and reported a fresh item as already triaged.
  function asJob(j: FlowJob): Job {
    return {
      id: j.job_id,
      kind: j.kind,
      title: j.title,
      status: j.status,
      steps: j.steps.map((s) => ({
        id: s.step_id,
        status: s.status,
        metadata: s.metadata,
        fields: s.field_names.map((name) => ({ name })),
      })),
    } as unknown as Job;
  }

  function forkFor(j: FlowJob): Fork | null {
    return forks[j.kind] ?? null;
  }

  /// When the decision was recorded, wall clock. The fork step's last
  /// write — for a completed step that is the completion.
  function disposedAt(j: FlowJob): number | null {
    const f = forkFor(j);
    const step = forkStep(asJob(j), f);
    if (!step) return null;
    const raw = j.steps.find((s) => s.step_id === step.id);
    if (!raw || (raw.status !== 'completed' && raw.status !== 'skipped')) return null;
    return raw.last_written_at ? Date.parse(raw.last_written_at) : null;
  }

  function filedAt(j: FlowJob): number | null {
    return j.filed_at ? Date.parse(j.filed_at) : null;
  }

  let now = $derived(flow ? Date.parse(flow.as_of) : Date.now());

  let inWindow = $derived.by(() =>
    (flow?.jobs ?? []).filter((j) => {
      if (windowHours === Number.POSITIVE_INFINITY) return true;
      const t = filedAt(j);
      return t !== null && now - t <= windowHours * 3_600_000;
    }),
  );

  let filed = $derived(inWindow.length);
  let triaged = $derived(inWindow.filter((j) => disposedAt(j) !== null).length);
  let waiting = $derived(inWindow.filter((j) => disposedAt(j) === null && j.status !== 'closed').length);
  let closed = $derived(inWindow.filter((j) => j.status === 'closed').length);

  /// Median, not mean: one item left overnight would drag an average
  /// far enough to hide that everything else was answered in minutes.
  let medianToDisposition = $derived.by(() => {
    const spans = inWindow
      .map((j) => {
        const a = filedAt(j);
        const b = disposedAt(j);
        return a !== null && b !== null && b >= a ? b - a : null;
      })
      .filter((v): v is number => v !== null)
      .sort((a, b) => a - b);
    if (spans.length === 0) return null;
    const mid = Math.floor(spans.length / 2);
    return spans.length % 2 ? spans[mid]! : Math.round((spans[mid - 1]! + spans[mid]!) / 2);
  });

  let byDisposition = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const j of inWindow) {
      const d = disposition(asJob(j), forkFor(j));
      if (d) counts.set(d, (counts.get(d) ?? 0) + 1);
    }
    // Every route the registry offers, including the ones nobody
    // chose — a disposition at zero is information ("we decline
    // nothing") that a filtered list would hide.
    const offered = new Set<string>();
    for (const f of Object.values(forks)) for (const o of f?.options ?? []) offered.add(o.value);
    for (const d of counts.keys()) offered.add(d);
    return [...offered]
      .map((value) => ({ value, count: counts.get(value) ?? 0 }))
      .sort((a, b) => b.count - a.count || a.value.localeCompare(b.value));
  });

  let maxDisposition = $derived(Math.max(1, ...byDisposition.map((d) => d.count)));

  /// Open items, oldest first — the ones a person is still waiting on.
  let openItems = $derived(
    inWindow
      .filter((j) => disposedAt(j) === null && j.status !== 'closed')
      .sort((a, b) => (filedAt(a) ?? 0) - (filedAt(b) ?? 0)),
  );

  function duration(ms: number | null): string {
    if (ms === null) return '—';
    const mins = Math.round(ms / 60_000);
    if (mins < 60) return `${mins}m`;
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    if (h < 24) return m ? `${h}h ${m}m` : `${h}h`;
    const d = Math.floor(h / 24);
    return `${d}d ${h % 24}h`;
  }

  async function load(): Promise<void> {
    loading = true;
    try {
      const [flowResp, kindsResp] = await Promise.all([
        fetch('/api/views/flow'),
        fetch('/api/jobs/kinds'),
      ]);
      if (!flowResp.ok) throw new Error(`flow: HTTP ${flowResp.status}`);
      const next: Flow = await flowResp.json();

      // One fork per kind: the dispositions a kind offers are its own,
      // and a page covering several must not assume they share one.
      const map: Record<string, Fork | null> = {};
      if (kindsResp.ok) {
        const rows = (await kindsResp.json()) as unknown[];
        for (const kind of next.kinds) {
          map[kind] = readFork(rows.find((k) => (k as { kind?: string }).kind === kind));
        }
      }
      forks = map;
      flow = next;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<PageHeader
  title="Flow"
  subtitle="What the team filed, routed and closed — and how long someone waited for an answer."
/>

{#if loading}
  <p class="flow-msg">Reading the queue…</p>
{:else if error}
  <p class="flow-msg flow-err">{error}</p>
{:else if flow}
  <div class="flow-bar">
    {#each WINDOWS as w (w.id)}
      <button
        type="button"
        class="flow-tab"
        class:flow-tab-on={windowId === w.id}
        onclick={() => (windowId = w.id)}
      >{w.label}</button>
    {/each}
    <span class="flow-scope">
      {flow.kinds.join(' · ')} — the workflows owned by {flow.owner_roles.join(', ')}
    </span>
  </div>

  <div class="flow-stats">
    <div class="flow-stat">
      <span class="flow-num">{filed}</span>
      <span class="flow-lab">Filed</span>
    </div>
    <div class="flow-stat">
      <span class="flow-num">{triaged}</span>
      <span class="flow-lab">Routed</span>
    </div>
    <div class="flow-stat" class:flow-warn={waiting > 0}>
      <span class="flow-num">{waiting}</span>
      <span class="flow-lab">Waiting on us</span>
    </div>
    <div class="flow-stat">
      <span class="flow-num">{closed}</span>
      <span class="flow-lab">Closed</span>
    </div>
    <div class="flow-stat flow-stat-wide">
      <span class="flow-num">{duration(medianToDisposition)}</span>
      <span class="flow-lab">Median time to a decision</span>
    </div>
  </div>

  <div class="flow-cols">
    <section class="flow-card">
      <h2 class="flow-h">Where it went</h2>
      {#if byDisposition.length === 0}
        <p class="flow-msg">Nothing routed in this window.</p>
      {:else}
        <ul class="flow-bars">
          {#each byDisposition as d (d.value)}
            <li class="flow-row">
              <span class="flow-key">{d.value}</span>
              <span class="flow-track">
                <span class="flow-fill" style="width: {(d.count / maxDisposition) * 100}%"></span>
              </span>
              <span class="flow-count">{d.count || '—'}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <section class="flow-card">
      <h2 class="flow-h">Waiting on us</h2>
      {#if openItems.length === 0}
        <p class="flow-msg">Nothing is waiting for a decision.</p>
      {:else}
        <ul class="flow-list">
          {#each openItems as j (j.job_id)}
            <li class="flow-item">
              <a class="flow-link" href="/jobs/{j.job_id}">{j.title}</a>
              <span class="flow-age">open {duration(now - (filedAt(j) ?? now))}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>

  <p class="flow-note">
    Times are real elapsed time, read from the audit log's write clock. The Jobs
    themselves are dated on the simulated calendar — event time is
    clock-authoritative, and on this deployment that clock is the simulator's —
    so a Job's own <code>opened_on</code> will not agree with the ages above.
    These numbers survive an epoch lap; the simulated company's do not.
  </p>
{/if}

<style>
  .flow-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  .flow-tab {
    font: inherit;
    font-size: 12px;
    padding: 3px 10px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .flow-tab-on {
    background: #0f766e;
    border-color: #0f766e;
    color: #fff;
  }
  .flow-scope {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
  }
  .flow-stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
    margin-bottom: 18px;
  }
  .flow-stat {
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 12px 14px;
    background: var(--card, #fff);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .flow-stat-wide {
    grid-column: span 2;
  }
  .flow-warn {
    border-color: #b45309;
    background: #fffbeb;
  }
  .flow-num {
    font-size: 26px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .flow-lab {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim, #78716c);
  }
  .flow-cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 12px;
  }
  .flow-card {
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 14px 16px;
    background: var(--card, #fff);
  }
  .flow-h {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim, #78716c);
    margin: 0 0 10px;
    font-weight: 600;
  }
  .flow-bars,
  .flow-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .flow-row {
    display: grid;
    grid-template-columns: 8.5rem 1fr 2rem;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }
  .flow-track {
    background: var(--bg, #f5f5f4);
    border-radius: 3px;
    height: 8px;
    overflow: hidden;
  }
  .flow-fill {
    display: block;
    height: 100%;
    background: #0f766e;
    border-radius: 3px;
  }
  .flow-count {
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim, #78716c);
  }
  .flow-item {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 10px;
    font-size: 13px;
  }
  .flow-link {
    color: inherit;
    text-decoration: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .flow-link:hover {
    text-decoration: underline;
  }
  .flow-age {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .flow-note {
    margin-top: 16px;
    font-size: 11px;
    line-height: 1.6;
    color: var(--text-dim, #a8a29e);
    max-width: 74ch;
  }
  .flow-note code {
    font-size: 11px;
  }
  .flow-msg {
    color: var(--text-dim, #78716c);
    font-size: 13px;
  }
  .flow-err {
    color: #b91c1c;
  }
</style>
