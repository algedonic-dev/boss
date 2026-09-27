<script lang="ts">
  // The estate — the hardware registry rendered instead of prose
  // (59ef456a: three hand-written accounts of the machines were wrong
  // the same way on 2026-08-30; this page reads the system so nobody
  // writes that doc again). Declared beside observed beside the
  // difference, then the loops that keep the estate (0d9b2960), and the
  // dev-workspace door at the bottom.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import {
    comparisonVerdict,
    DEV_DOOR_HOST,
    devDoorSteps,
    fetchEstate,
    freshnessText,
    hostCoverText,
    hostLines,
    latestComparison,
    LOOP_OK_OUTCOMES,
    loopAge,
    loopHost,
    missingHostText,
    seriesAbsentText,
    seriesFreshness,
    unitsVerdict,
    type EstateState,
    type Observation,
  } from './estate';

  let estate = $state<EstateState | null>(null);
  // One clock for every relative stamp on the page, taken when the
  // data arrived — formatRelative takes `now` explicitly, no hidden
  // wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    estate = await fetchEstate();
    loadedAt = new Date();
  }

  onMount(() => {
    void refresh();
    // Slow refresh: the estate changes on the order of days; 60s keeps
    // the relative stamps honest without hammering guest reads.
    const t = setInterval(() => void refresh(), 60_000);
    return () => clearInterval(t);
  });

  // Each series' age judged against its OWN cadence, by the alarm's
  // test (e1eb34bc): past three of them the stamp reads amber and says
  // so, because the alarm has filed — or is about to file — that series
  // as unobserved.
  function when(rows: readonly Observation[]): { stale: boolean; text: string } {
    const f = seriesFreshness(rows, loadedAt);
    return f ? { stale: f.state === 'stale', text: freshnessText(f) } : { stale: false, text: '' };
  }
  const clusterCmp = $derived(
    estate?.comparisons.kind === 'ready' ? latestComparison(estate.comparisons.data, 'kubernetes-nodes') : null,
  );
  // The one-time terminal setup, spelled by the module that holds the
  // hostname — no second address typed into this file.
  const doorSteps = devDoorSteps();
</script>

<div class="estate-root">
  <PageHeader
    eyebrow="IT · Hardware"
    title="The estate"
    subtitle="Declared beside observed — what we meant to have, what a look found, and the difference"
  />

  {#if !estate}
    <p class="estate-quiet">Reading the registry…</p>
  {:else}
    <div class="estate-section">00 — THE MACHINES</div>
    {#if estate.nodes.kind === 'failed'}
      <p class="estate-fail load-failed">The registry did not answer: {estate.nodes.error}. This page refuses to guess — an unreachable registry is not an empty estate.</p>
    {:else if estate.nodes.kind === 'ready'}
      <table class="estate-table">
        <thead>
          <tr><th>machine</th><th>role</th><th>address</th><th>cpu</th><th>mem</th><th>disk</th></tr>
        </thead>
        <tbody>
          {#each estate.nodes.data.filter((n) => !n.retired) as n (n.id)}
            <tr title={n.notes ?? ''}>
              <td class="estate-id">{n.id}</td>
              <td>
                {n.role}{#if n.roles.length > 0}<span class="estate-roles"> · {n.roles.join(' · ')}</span>{/if}
              </td>
              <td class="estate-addr">{n.address ?? '—'}</td>
              <td class="estate-num">{n.cpu ?? '—'}</td>
              <td class="estate-num">{n.memory_gb != null ? `${n.memory_gb}G` : '—'}</td>
              <td class="estate-num">{n.disk_gb != null ? `${n.disk_gb}G` : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <div class="estate-section">01 — OBSERVED vs DECLARED</div>
    <!-- ONE READ PER SERIES (75027a93): the cluster's, and per host its
         `host` and `host-units` series. Each series fails, or says it
         was never recorded, on its own line — and "never recorded" only
         when its own read counted none. Until this, one unscoped page of
         20 rows fed every line, and a slow series fell off it. -->
    <div class="estate-obs">
      {#if estate.cluster.kind === 'failed'}
        <p class="estate-fail load-failed">Observations unavailable: {estate.cluster.error}</p>
      {:else if estate.cluster.kind === 'ready'}
        {@const clusterObs = estate.cluster.data.rows[0]}
        {#if clusterObs}
          {@const w = when(estate.cluster.data.rows)}
          <div class="estate-obs-row">
            <span class="estate-scope">kubernetes-nodes</span>
            <span>
              {clusterObs.nodes.length} machines seen by {clusterObs.observer}
              <!-- Free space per node, same idiom as the host row below.
                   Until a520737f this scope recorded capacity only, so the
                   disk floor could never fire for a cluster node — w-1
                   included, the node every gate compiles on. A node whose
                   kubelet read failed says so rather than going quiet. -->
              {#each clusterObs.nodes as cn (cn.id)}
                — {cn.id}: {cn.disk_free_gb != null ? `${cn.disk_free_gb}G free` : 'free space unread'}
              {/each}
            </span>
            <span class={w.stale ? 'estate-when estate-drift' : 'estate-when'}>{w.text}</span>
          </div>
        {:else}
          <div class="estate-obs-row"><span class="estate-scope">kubernetes-nodes</span><span>{seriesAbsentText(estate.cluster.data)}</span></div>
        {/if}
      {/if}
      {#if !estate.hosts.known}
        <!-- Neither the registry nor the host comparisons answered (both
             say so above), so no host series could be named to read. -->
        <div class="estate-obs-row"><span class="estate-scope">host</span><span class="estate-drift">no host series read — neither the registry nor the host comparisons answered</span></div>
      {/if}
      <!-- One line per host, each its own series' newest reading and its
           own age (3d1678ba); a missing reading says so. -->
      {#each estate.hosts.series as hs (hs.host)}
        {#if hs.readings.kind === 'failed'}
          <p class="estate-fail load-failed">Observations unavailable: {hs.readings.error}</p>
        {:else if hs.readings.kind === 'ready'}
          {@const hr = hs.readings.data.rows[0]}
          {#if hr}
            {@const w = when(hs.readings.data.rows)}
            <div class="estate-obs-row">
              <span class="estate-scope">host</span>
              <span>
                {hs.host}: {hr.nodes[0]?.disk_free_gb != null ? `${hr.nodes[0]?.disk_free_gb}G free` : 'free space unread'} — seen by {hr.observer}
              </span>
              <span class={w.stale ? 'estate-when estate-drift' : 'estate-when'}>{w.text}</span>
            </div>
          {:else}
            <div class="estate-obs-row"><span class="estate-scope">host</span><span>{hs.host}: {seriesAbsentText(hs.readings.data)}</span></div>
          {/if}
        {/if}
      {:else}
        {#if estate.hosts.known}
          <div class="estate-obs-row"><span class="estate-scope">host</span><span>no host declared or compared, so no host series to read</span></div>
        {/if}
      {/each}
      <!-- UNIT HEALTH (d5efb80d): each host's newest host-units reading —
           how many units it watched and which are unhealthy, by name. The
           series was fetched and discarded until this line. -->
      {#each estate.hosts.series as hs (hs.host)}
        {#if hs.units.kind === 'failed'}
          <p class="estate-fail load-failed">Observations unavailable: {hs.units.error}</p>
        {:else if hs.units.kind === 'ready'}
          {@const ur = hs.units.data.rows[0]}
          {#if ur}
            {@const v = unitsVerdict(ur.nodes[0] ?? { id: hs.host })}
            {@const w = when(hs.units.data.rows)}
            <div class="estate-obs-row">
              <span class="estate-scope">host units</span>
              <span class={v.ok ? 'estate-ok' : 'estate-drift'}>{hs.host}: {v.text}</span>
              <span class={w.stale ? 'estate-when estate-drift' : 'estate-when'}>{w.text}</span>
            </div>
          {:else}
            <div class="estate-obs-row"><span class="estate-scope">host units</span><span>{hs.host}: {seriesAbsentText(hs.units.data)}</span></div>
          {/if}
        {/if}
      {/each}
      {#if estate.comparisons.kind === 'failed'}
        <p class="estate-fail load-failed">Comparisons unavailable: {estate.comparisons.error}</p>
      {:else if clusterCmp}
        {@const v = comparisonVerdict(clusterCmp)}
        <div class="estate-obs-row">
          <span class="estate-scope">comparison</span>
          <span class={v.ok ? 'estate-ok' : 'estate-drift'}>{v.text}</span>
          <span class="estate-when">{formatRelative(clusterCmp.observed_at, loadedAt)}</span>
        </div>
      {/if}
      <!-- THE HOST COMPARISON (backlog 2d8d983b, page audit 2cff1d6e
           GAP 1): each host's newest self-scoped comparison, from its
           own scoped read. Until this, every host row's drift and
           boss-gcp's disk_tight were recorded and never shown here. -->
      {#if estate.hostComparisons.kind === 'failed'}
        <p class="estate-fail load-failed">Host comparisons unavailable: {estate.hostComparisons.error}</p>
      {:else if estate.hostComparisons.kind === 'ready'}
        {@const hc = estate.hostComparisons.data}
        <!-- Every declared host gets its line (backlog 725532ab): the
             read is grouped per host on the server, so a daily host is
             no longer spent off the page by a fifteen-minute one, and a
             declared host with no row says so in amber. -->
        {#each hostLines(estate.nodes, hc) as l (l.host ?? '')}
          {#if l.cmp}
            {@const v = comparisonVerdict(l.cmp)}
            <div class="estate-obs-row">
              <span class="estate-scope">host comparison</span>
              <span class={v.ok ? 'estate-ok' : 'estate-drift'}>{l.host ?? 'host not named on the row'}: {v.text}</span>
              <span class="estate-when">{formatRelative(l.cmp.observed_at, loadedAt)}</span>
            </div>
          {:else}
            <div class="estate-obs-row">
              <span class="estate-scope">host comparison</span>
              <span class="estate-drift">{l.host}: {missingHostText(hc)}</span>
            </div>
          {/if}
        {:else}
          <div class="estate-obs-row"><span class="estate-scope">host comparison</span><span>no host comparison recorded yet</span></div>
        {/each}
        {@const cover = hostCoverText(hc)}
        {#if cover}
          <!-- A limit is not a filter: a read that is not every host
               says how much of the series it holds. -->
          <p class="estate-cover">{cover}</p>
        {/if}
      {/if}
    </div>

    <!-- THE LOOPS (backlog 0d9b2960, page audit 2cff1d6e GAP 10): the
         packets the estate's own loops leave, so "did the loop run" is
         answered here. Every cell is its own read, and a failed read says
         so in that cell — an unread loop is not a loop that did not run. -->
    <div class="estate-section">02 — THE LOOPS</div>
    <p class="estate-hint">
      Did each loop run: its newest finished packet (outcome and age) and any packet still open,
      each linked. The host is the one the packet names; where a packet names none, the page says so
      rather than guess.
    </p>
    <table class="estate-table estate-loops">
      <thead>
        <tr><th>loop</th><th>host</th><th>newest finished</th><th>open</th></tr>
      </thead>
      <tbody>
        {#each estate.loops as l (`${l.kind}:${l.host ?? ''}`)}
          <tr data-loop={l.kind}>
            <td class="estate-id">{l.label}</td>
            <td class="estate-addr">{loopHost(l)}</td>
            <td class="estate-loop-latest">
              {#if l.latest.kind === 'failed'}
                <span class="estate-drift load-failed">unread: {l.latest.error}</span>
              {:else if l.latest.kind === 'ready'}
                {#if l.latest.data}
                  {@const p = l.latest.data}
                  <a class={LOOP_OK_OUTCOMES.has(p.outcome ?? '') ? 'estate-ok' : 'estate-drift'} href={`/ux/jobs/${p.id}`}>{p.outcome ?? p.status}</a>
                  <span class="estate-age">{loopAge(p.at, loadedAt)}</span>
                {:else}
                  <span class="estate-drift">no finished run recorded</span>
                {/if}
              {/if}
            </td>
            <td class="estate-loop-open">
              {#if l.open.kind === 'failed'}
                <span class="estate-drift load-failed">unread: {l.open.error}</span>
              {:else if l.open.kind === 'ready'}
                {#each l.open.data as o (o.id)}
                  <a href={`/ux/jobs/${o.id}`}>open</a>
                  <span class="estate-age">{loopAge(o.at, loadedAt)}</span>
                {:else}
                  <span class="estate-quiet">none</span>
                {/each}
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>

    <div class="estate-section">03 — THE DEV WORKSPACE</div>
    <div class="estate-door">
      <p class="estate-hint">
        The workspace answers on <code>{DEV_DOOR_HOST}</code>, from anywhere, behind Cloudflare
        Access. There is no VPN to join and no key to install: the edge asks who you are and issues
        a certificate that lasts the session. Three lines, the first two once per machine.
      </p>
      {#each doorSteps as step, i (step.command)}
        <p class="estate-hint"><strong>{i + 1}. {step.what}</strong> — {step.why}</p>
        <pre class="estate-snippet">{step.command}</pre>
      {/each}
      <p class="estate-hint">
        Inside: the durable tmux session is <code>dev</code> — attach with
        <code>/work/dev-session.sh</code>, detach with <code>ctrl-b d</code>. For the browser
        instead, run <code>claude remote-control</code> inside the session and drive it from
        claude.ai.
      </p>
    </div>
  {/if}
</div>

<style>
  .estate-root { padding: 0 32px 32px; }
  .estate-section {
    font-family: var(--font-mono);
    font-size: 12px; letter-spacing: var(--ls-eyebrow);
    color: var(--signal); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .estate-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline); }
  .estate-quiet { color: var(--static); }
  .estate-fail {
    color: var(--warn);
    border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .estate-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .estate-table th {
    text-align: left; font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static); font-weight: 400;
    border-bottom: 1px solid var(--hairline); padding: 4px 12px 4px 0;
  }
  .estate-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline); }
  .estate-id { font-family: var(--font-mono); }
  .estate-addr, .estate-num { font-family: var(--font-mono); color: var(--static); }
  .estate-roles { font-family: var(--font-mono); font-size: 11px; color: var(--static); }
  .estate-obs { display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
  .estate-obs-row { display: flex; gap: 16px; align-items: baseline; }
  .estate-scope {
    font-family: var(--font-mono); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static);
    min-width: 150px;
  }
  .estate-when { color: var(--static); font-size: 12px; margin-left: auto; }
  .estate-age { color: var(--static); font-size: 12px; margin-left: 8px; }
  .estate-ok { color: var(--signal); }
  .estate-drift { color: var(--warn); }
  .estate-door { display: flex; flex-direction: column; gap: 8px; }
  .estate-hint, .estate-cover { color: var(--static); font-size: 12px; max-width: 60ch; }
  .estate-cover { margin: 0; }
  .estate-hint code { font-family: var(--font-mono); }
  .estate-hint strong { color: var(--fog); font-weight: 500; }
  /* One click selects the whole snippet — copyable without a button. */
  .estate-snippet {
    font-family: var(--font-mono); font-size: 12px;
    color: var(--signal); border: 1px solid var(--hairline);
    padding: 8px 14px; margin: 0; width: fit-content; user-select: all;
  }
</style>
