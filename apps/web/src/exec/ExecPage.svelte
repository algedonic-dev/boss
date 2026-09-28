<script lang="ts">
  // Executive overview — port of apps/web/src/exec/ExecPage.tsx.
  //
  // Inlines its panels because each one is small and they share the
  // dashboard-card layout. The reads, the line each read shows and each
  // card's link are data in ./exec.ts, pinned by ExecPage.test.ts; this
  // file renders what that module answers and nothing of its own
  // (page audit a1d62870, 2026-09-27). The launch-calendar panel and
  // the people read that named its owners retired with the second
  // example tenant (design 2ea444f5, backlog a8991c86); the
  // Finished-goods panel retired with the audit (6775546c): no protocol
  // of ours produces finished goods, and its link led to a page gated
  // on a module our instance does not run.
  //
  // `department` is the catalog entry's `owner`, handed in by App.svelte
  // the way FinancePage takes its own: the Executive work card is that
  // department's in / working / out (1f33e125).

  import Link from '@boss/web-kit/ui/Link.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { href } from '../router';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import DepartmentThirds from '../departments/DepartmentThirds.svelte';
  import type { Remote } from '../data/remote';
  import {
    EXEC_LINES,
    EXEC_LINKS,
    JOBS_SCOPE_NOTE,
    executiveWorkLink,
    kindLabelOf,
    loadBalances,
    loadJobsSummary,
    loadKindLabels,
    loadRevenueMix,
    loadWaiting,
    noteOf,
    trailingYear,
    waitingText,
    type Balances,
    type JobsSummary,
    type Note,
    type RevenueMix,
    type Waiting,
  } from './exec';

  let { department = '' }: { department?: string } = $props();

  function fmtUsd(cents: number): string {
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'compact' });
  }
  // $derived so the displayed date updates as the sim clock
  // advances. Pre-fix this was a plain function called from the
  // template (`{todayStr()}`); Svelte 5 sometimes doesn't track
  // the cross-module $state read through a function call, so
  // the date appeared frozen even as simClock.value updated via
  // SSE. $derived registers the dep explicitly at the component
  // scope and re-runs on every simClock update.
  const todayStr = $derived(
    appNow().toLocaleDateString('en-US', {
      weekday: 'long',
      month: 'long',
      day: 'numeric',
      year: 'numeric',
    }),
  );

  // --- Reads — each a Remote, each answered on its own ------------------

  let waiting = $state<Remote<Waiting>>({ kind: 'loading' });
  let revenue = $state<Remote<RevenueMix>>({ kind: 'loading' });
  let balance = $state<Remote<Balances>>({ kind: 'loading' });
  let jobs = $state<Remote<JobsSummary>>({ kind: 'loading' });
  let kindLabels = $state<ReadonlyMap<string, string>>(new Map());

  // The trailing year ends on the app's today, so a scrubbed or sim
  // clock reads the ledger over the same year the page says it is.
  const ttm = $derived(trailingYear(appToday()));

  $effect(() => {
    let cancelled = false;
    const w = ttm;
    void loadWaiting().then((r) => {
      if (!cancelled) waiting = r;
    });
    void loadRevenueMix(w).then((r) => {
      if (!cancelled) revenue = r;
    });
    void loadBalances().then((r) => {
      if (!cancelled) balance = r;
    });
    void loadJobsSummary().then((r) => {
      if (!cancelled) jobs = r;
    });
    void loadKindLabels().then((m) => {
      if (!cancelled) kindLabels = m;
    });
    return () => {
      cancelled = true;
    };
  });

  const jobTop = $derived(jobs.kind === 'ready' ? jobs.data.byKind.slice(0, 5) : []);
  const jobMaxCount = $derived(Math.max(...jobTop.map((r) => r.count), 1));
  const workLink = $derived(executiveWorkLink(department));

  // The line each card shows in place of its data, or null when it has
  // data to show: loading, failed (on the shared marker) or truly empty.
  const waitingNote = $derived(noteOf(waiting, EXEC_LINES.waiting));
  const revenueNote = $derived(noteOf(revenue, EXEC_LINES.revenue));
  const balanceNote = $derived(noteOf(balance, EXEC_LINES.balance));
  const jobsNote = $derived(noteOf(jobs, EXEC_LINES.jobs));
</script>

{#snippet noteLine(n: Note)}
  {#if n.failed}
    <p class="empty load-failed" role="alert">{n.text}</p>
  {:else}
    <p class="empty">{n.text}</p>
  {/if}
{/snippet}

<div class="exec theme-exec">
  <header class="exec-header">
    <div>
      <div class="exec-eyebrow">BOSS — Executive Overview</div>
      <h1 class="exec-title">{todayStr}</h1>
    </div>
    <!-- Proposed wording for the review step (faf3f635): "What changed"
         is gone because no card shows a change feed. -->
    <div class="exec-subtitle">Money, open work, and what is waiting on you</div>
  </header>

  <div class="exec-grid">
    <section class="exec-card exec-card-accent">
      <h2>Waiting on you</h2>
      {#if waitingNote}
        {@render noteLine(waitingNote)}
      {:else if waiting.kind === 'ready'}
        <p class="waiting-count">{waitingText(waiting.data)}</p>
      {/if}
      <div class="card-link">
        <Link to={href(EXEC_LINKS.waiting.to)}>{EXEC_LINKS.waiting.label}</Link>
      </div>
    </section>

    <section class="exec-card">
      <h2>Revenue mix — trailing 12 months</h2>
      {#if revenueNote}
        {@render noteLine(revenueNote)}
      {:else if revenue.kind === 'ready'}
        <div class="stat-row">
          <div class="stat">
            <div class="stat-label">Total revenue</div>
            <div class="stat-value">{fmtUsd(revenue.data.total)}</div>
          </div>
        </div>
        <div class="mix">
          {#each revenue.data.rows as r (r.code)}
            <div class="mix-row">
              <div class="mix-label">{r.name}</div>
              <div class="mix-bar">
                <div class="mix-fill" style={`width:${r.share * 100}%`}></div>
              </div>
              <div class="mix-share">{(r.share * 100).toFixed(0)}%</div>
              <div class="mix-amount">{fmtUsd(r.amount)}</div>
            </div>
          {/each}
        </div>
      {/if}
      <div class="card-link">
        <Link to={href(EXEC_LINKS.revenue.to)}>{EXEC_LINKS.revenue.label}</Link>
      </div>
    </section>

    <section class="exec-card">
      <h2>Cash &amp; receivables</h2>
      {#if balanceNote}
        {@render noteLine(balanceNote)}
      {:else if balance.kind === 'ready'}
        <div class="as-of">As of {balance.data.asOf}</div>
        <div class="stat-row">
          {#each balance.data.stats as s (s.label)}
            <div class="stat">
              <div class="stat-label">{s.label}</div>
              <div class="stat-value">{fmtUsd(s.cents)}</div>
            </div>
          {/each}
        </div>
      {/if}
      <div class="card-link">
        <Link to={href(EXEC_LINKS.balance.to)}>{EXEC_LINKS.balance.label}</Link>
      </div>
    </section>

    <section class="exec-card exec-card-wide">
      <h2>Active jobs</h2>
      {#if jobsNote}
        {@render noteLine(jobsNote)}
      {:else if jobs.kind === 'ready'}
        <div class="stat-row">
          <div class="stat">
            <div class="stat-label">Open jobs</div>
            <div class="stat-value">{jobs.data.total.toLocaleString()}</div>
          </div>
          <div class="stat">
            <div class="stat-label">Job kinds active</div>
            <div class="stat-value">{jobs.data.byKind.length}</div>
          </div>
        </div>
        <!-- 1445813b: the summary is scoped to the caller since 19f08bd6;
             a count that does not say whose it is reads as the company's. -->
        <p class="empty scope-note">{JOBS_SCOPE_NOTE}</p>
        <div class="bar-list">
          {#each jobTop as r (r.kind)}
            <div class="bar-row">
              <span class="bar-label">{kindLabelOf(kindLabels, r.kind)}</span>
              <div class="bar-track">
                <div class="bar-fill" style={`width:${(r.count / jobMaxCount) * 100}%`}></div>
              </div>
              <span class="bar-count">{r.count}</span>
            </div>
          {/each}
        </div>
      {/if}
      <div class="card-link">
        <Link to={href(EXEC_LINKS.jobs.to)}>{EXEC_LINKS.jobs.label}</Link>
      </div>
    </section>

    <section class="exec-card exec-card-wide">
      <h2>Executive work</h2>
      <!-- The department's own protocols, in / working / out: the
           shared component says a failed read is a failure, never
           "nothing open" (1f33e125). -->
      <DepartmentThirds code={department} />
      <div class="card-link">
        <Link to={href(workLink.to)}>{workLink.label}</Link>
      </div>
    </section>
  </div>

  <footer class="exec-footer">
    Executive overview — each card links to the deeper view.
  </footer>
</div>

<style>
  .card-link {
    margin-top: 12px;
    text-align: right;
  }
  .as-of {
    margin-bottom: 12px;
    font-size: 13px;
    color: var(--static);
  }
  .waiting-count {
    margin: 0;
  }
</style>
