<script lang="ts">
  // The estate — the hardware registry rendered instead of prose
  // (59ef456a: three hand-written accounts of the machines were wrong
  // the same way on 2026-08-30; this page reads the system so nobody
  // writes that doc again). Declared beside observed beside the
  // difference, then the loops that keep the estate (0d9b2960), the edge
  // in front of it — zone, Access, tunnel (e0e183fb) — and the
  // dev-workspace door at the bottom.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import {
    alarmCoverText,
    alarmsOn,
    CLUSTER_SCOPE,
    comparisonVerdict,
    capacityIntentLine,
    HOST_SCOPE,
    MACHINE_FIELDS,
    machineDrift,
    machineSight,
    machineValue,
    seenCell,
    UNITS_SCOPE,
    devDoorSteps,
    fetchDevDoor,
    fetchEstate,
    freshnessText,
    hostCoverText,
    hostLines,
    latestComparison,
    LOOP_OK_OUTCOMES,
    loopAge,
    loopHost,
    missingHostText,
    registryFailure,
    seriesAbsentText,
    seriesFreshness,
    tunnelLine,
    unitsVerdict,
    volumeAlarms,
    volumeLine,
    zoneAlarms,
    zoneVerdict,
    type EstateState,
    type DevDoorDeclaration,
  } from './estate';
  import type { Remote } from '../../data/remote';

  let estate = $state<EstateState | null>(null);
  let door = $state<Remote<DevDoorDeclaration>>({ kind: 'loading' });
  // One clock for every relative stamp on the page, taken when the
  // data arrived — formatRelative takes `now` explicitly, no hidden
  // wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    const [nextEstate, nextDoor] = await Promise.all([fetchEstate(), fetchDevDoor()]);
    estate = nextEstate;
    door = nextDoor;
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
  function when(rows: readonly Readonly<{ observed_at: string }>[]): { stale: boolean; text: string } {
    const f = seriesFreshness(rows, loadedAt);
    return f ? { stale: f.state === 'stale', text: freshnessText(f) } : { stale: false, text: '' };
  }
  // The tunnel routes, off the cluster converge's loop row (THE EDGE).
  const tunnel = $derived(estate ? tunnelLine(estate.loops) : null);
  const clusterCmp = $derived(
    estate?.comparisons.kind === 'ready' ? latestComparison(estate.comparisons.data, 'kubernetes-nodes') : null,
  );
  // The one-time terminal setup, spelled by the module that holds the
  // hostname — no second address typed into this file.
  const doorSteps = $derived(door.kind === 'ready' ? devDoorSteps(door.data.host, door.data.steps) : []);
</script>

<!-- The open alarms on one verdict line's series (48ef9961), each a link
     to its packet, its title as the tooltip. -->
{#snippet alarmLinks(scope: string, host: string | null)}
  {#each estate ? alarmsOn(estate.alarms, scope, host) : [] as a (a.id)}
    <a class="estate-alarm-link" href={`/ux/jobs/${a.id}`} title={a.title}>alarm</a>
  {/each}
{/snippet}

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
      <p class="estate-fail load-failed">{registryFailure(estate.nodes.error)}</p>
    {:else if estate.nodes.kind === 'ready'}
      {@const activeMachines = estate.nodes.data.filter((n) => !n.retired)}
      <p class="estate-machine-count estate-quiet">
        {estate.nodes.data.length} machine{estate.nodes.data.length === 1 ? '' : 's'} declared · {activeMachines.length} active · {estate.nodes.data.length - activeMachines.length} retired
      </p>
      <!-- DECLARED BESIDE OBSERVED (ab3c54d7): each value cell is the
           declared value, then what the newest reading of the machine's
           own series saw; a field the newest comparison names as drift
           reads amber, with both sides in its tooltip. -->
      <p class="estate-legend">
        Each value is what the registry declares, then what the newest observation saw. A field a
        comparison names as drift reads amber.
      </p>
      <table class="estate-table">
        <thead>
          <tr><th>machine</th><th>role</th><th>address</th><th>cpu</th><th>mem</th><th>disk</th></tr>
        </thead>
        <tbody>
          {#each activeMachines as n (n.id)}
            {@const sight = machineSight(estate, n)}
            {@const drift = machineDrift(estate, n.id)}
            <tr title={n.notes ?? ''}>
              <td class="estate-id">{n.id}</td>
              <td>
                {n.role}{#if n.roles.length > 0}<span class="estate-roles"> · {n.roles.join(' · ')}</span>{/if}
              </td>
              {#each MACHINE_FIELDS as f (f)}
                {@const seen = seenCell(sight, drift, f)}
                <td
                  class={f === 'address' ? 'estate-addr' : 'estate-num'}
                  data-drift={seen.drift ? f : undefined}
                  title={seen.drift ? `drift: declared ${machineValue(f, seen.drift.declared)}, observed ${machineValue(f, seen.drift.observed)}` : undefined}
                >
                  {machineValue(f, n[f])} <span class={seen.drift ? 'estate-seen estate-drift' : 'estate-seen'}>{seen.text}</span>
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    <!-- THE INSTANCE VOLUMES (backlog 21ee3b4e, incident d3c0a67c): every
         claim in every instance namespace, beside the machines that hold
         them. The system of record's database volume filled on
         2026-10-01 with nothing watching; each row is the comparator's
         verdict on the forge's newest reading, a claim it could not read
         says unread, and an open alarm on a claim is linked on its row. -->
    <div class="estate-volumes">
      {#if estate.volumes.kind === 'failed'}
        <p class="estate-fail load-failed">Volume readings unavailable: {estate.volumes.error}</p>
      {:else if estate.volumes.kind === 'ready'}
        {@const vr = estate.volumes.data[0]}
        {#if vr}
          {@const w = when(estate.volumes.data)}
          <div class="estate-volumes-head">
            <span>Instance volumes — {vr.volumes.length} claims read by {vr.observer}</span>
            <span class={w.stale ? 'estate-when estate-drift' : 'estate-when'}>{w.text}</span>
          </div>
          <table class="estate-volume-table">
            <thead>
              <tr><th>claim</th><th>volume</th><th>free</th><th>declared capacity · report only</th></tr>
            </thead>
            <tbody>
              {#each vr.volumes as v (v.id)}
                {@const line = volumeLine(v)}
                <tr data-volume={v.id} data-state={line.state}>
                  <td class="estate-id">{v.id}</td>
                  <td class="estate-addr">{v.volume ?? '—'}</td>
                  <td class={line.state === 'ok' ? 'estate-ok' : 'estate-drift'}>
                    {line.text}
                    {#each volumeAlarms(estate.alarms, v.id) as a (a.id)}
                      <a class="estate-alarm-link" href={`/ux/jobs/${a.id}`} title={a.title}>alarm</a>
                    {/each}
                  </td>
                  <td class={v.capacity_intent.verdict === 'match' ? 'estate-ok' : 'estate-drift'} data-capacity-intent={v.capacity_intent.verdict}>
                    {capacityIntentLine(v)}
                    {#if v.capacity_intent.assignment}
                      <a class="estate-alarm-link" href={`/ux/jobs/${v.capacity_intent.assignment.packet}`} title={`Assigned by ${v.capacity_intent.assignment.decided_by} at ${v.capacity_intent.assignment.decided_at}; step ${v.capacity_intent.assignment.step}`}>
                        assignment {v.capacity_intent.assignment.question}
                      </a>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <p class="estate-volumes-none">Instance volumes: no volume reading recorded yet.</p>
        {/if}
      {/if}
    </div>

    <div class="estate-section">01 — OBSERVED vs DECLARED</div>
    <!-- THE OPEN ALARMS (48ef9961): every open packet estate.alarm filed,
         linked, above the lines they explain — and each also beside the
         verdict of its own series below. The page had no link to one. -->
    <div class="estate-alarms">
      {#if estate.alarms.kind === 'failed'}
        <p class="estate-fail load-failed">Estate alarms unavailable: {estate.alarms.error}</p>
      {:else if estate.alarms.kind === 'ready'}
        {#each estate.alarms.data.rows as a (a.id)}
          <div class="estate-alarm-row">
            <span class="estate-scope">open alarm</span>
            <a class="estate-drift" href={`/ux/jobs/${a.id}`}>{a.title}</a>
            <span class="estate-when">{loopAge(a.at, loadedAt)}</span>
          </div>
        {:else}
          {#if !estate.alarms.data.total}
            <p class="estate-alarm-none">No estate alarm is open.</p>
          {/if}
        {/each}
        {@const alarmCover = alarmCoverText(estate.alarms.data)}
        {#if alarmCover}
          <p class="estate-alarm-cover">{alarmCover}</p>
        {/if}
      {/if}
    </div>
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
              {@render alarmLinks(UNITS_SCOPE, hs.host)}
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
          {@render alarmLinks(CLUSTER_SCOPE, null)}
          <span class="estate-when">{formatRelative(clusterCmp.observed_at, loadedAt)}</span>
        </div>
      {:else if estate.comparisons.kind === 'ready'}
        <div class="estate-obs-row"><span class="estate-scope">comparison</span><span>0 cluster comparisons in this read</span></div>
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
              {@render alarmLinks(HOST_SCOPE, l.host)}
              <span class="estate-when">{formatRelative(l.cmp.observed_at, loadedAt)}</span>
            </div>
          {:else}
            <div class="estate-obs-row">
              <span class="estate-scope">host comparison</span>
              <span class="estate-drift">{l.host}: {missingHostText(hc)}</span>
              {@render alarmLinks(HOST_SCOPE, l.host)}
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

    <!-- THE EDGE (backlog e0e183fb, page audit 2cff1d6e GAP 11): the
         DNS zone, the Access applications in front of it and the tunnel
         routes behind it — declared in the tree, read back and compared
         by the daily zone observation and the cluster converge, and
         rendered here off those packets. A zone's alarm sits on its own
         line. Nothing here is spelled by the page: every name is the
         packet's. -->
    <div class="estate-section">03 — THE EDGE</div>
    <p class="estate-hint">
      What the declaration says beside what the newest reading found: each zone record, each Access
      application in front of it, and the tunnel routes behind them. A record or application a
      reading names as drift reads amber.
    </p>
    <div class="estate-edge">
      {#if estate.zones.open.kind === 'failed'}
        <p class="estate-fail load-failed">Open zone readings unavailable: {estate.zones.open.error}</p>
      {:else if estate.zones.open.kind === 'ready'}
        {#each estate.zones.open.data as o (o.id)}
          <div class="estate-obs-row" data-zone-open={o.zone}>
            <span class="estate-scope">zone</span>
            <a class="estate-drift" href={`/ux/jobs/${o.id}`}>{o.zone}: a reading is still open — the zone could not be read or compared</a>
            <span class="estate-when">{loopAge(o.at, loadedAt)}</span>
          </div>
        {/each}
      {/if}
      {#if estate.zones.latest.kind === 'failed'}
        <p class="estate-fail load-failed">Zone readings unavailable: {estate.zones.latest.error}</p>
      {:else if estate.zones.latest.kind === 'ready'}
        {#each estate.zones.latest.data as z (z.zone)}
          {@const v = zoneVerdict(z)}
          <div class="estate-obs-row estate-zone" data-zone={z.zone}>
            <span class="estate-scope">zone</span>
            <a class={v.ok ? 'estate-ok' : 'estate-drift'} href={`/ux/jobs/${z.id}`}>{z.zone}: {v.text}</a>
            {#each zoneAlarms(estate.alarms, z.zone) as a (a.id)}
              <a class="estate-alarm-link" href={`/ux/jobs/${a.id}`} title={a.title}>alarm</a>
            {/each}
            <span class="estate-when">{loopAge(z.at, loadedAt)}</span>
          </div>
          <table class="estate-edge-table estate-records">
            <thead>
              <tr><th>record</th><th>declared</th><th>in front</th><th>verdict</th></tr>
            </thead>
            <tbody>
              {#each z.records as r, i (`${r.record}#${i}`)}
                <tr title={r.why ?? ''}>
                  <td class="estate-id">{r.record}</td>
                  <td class="estate-addr">
                    {r.declared ?? '—'}
                    <span class={r.verdict === 'MATCH' ? 'estate-seen' : 'estate-seen estate-drift'}>{r.live === null ? 'not in the zone' : `zone holds ${r.live}`}</span>
                  </td>
                  <td class="estate-addr">{r.front ?? '—'}</td>
                  <td class={r.verdict === 'MATCH' ? 'estate-ok' : 'estate-drift'}>{r.verdict}{r.note ? ` — ${r.note}` : ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          {#if z.undeclared > 0}
            <p class="estate-edge-note">{z.undeclared} live record{z.undeclared === 1 ? '' : 's'} the declaration names nowhere — reported on the reading, not a finding.</p>
          {/if}
          <table class="estate-edge-table estate-access">
            <thead>
              <tr><th>access application</th><th>type</th><th>policies</th><th>verdict</th></tr>
            </thead>
            <tbody>
              {#each z.access as a, i (`${a.domain}#${i}`)}
                <tr title={a.why ?? ''}>
                  <td class="estate-id">{a.domain}</td>
                  <td class="estate-addr">{a.type ?? '—'}</td>
                  <td class="estate-addr">{a.policies.length > 0 ? a.policies.join(' · ') : '—'}</td>
                  <td class={a.verdict === 'MATCH' ? 'estate-ok' : 'estate-drift'}>{a.verdict}{a.note ? ` — ${a.note}` : ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          {#if z.accessUndeclared > 0}
            <p class="estate-edge-note">{z.accessUndeclared} Access application{z.accessUndeclared === 1 ? '' : 's'} the declaration names nowhere.</p>
          {/if}
        {:else}
          <div class="estate-obs-row"><span class="estate-scope">zone</span><span>no zone reading recorded yet</span></div>
        {/each}
      {/if}
      {#if tunnel}
        <div class="estate-obs-row estate-tunnel">
          <span class="estate-scope">tunnel routes</span>
          {#if tunnel.id}
            <a class={tunnel.ok ? 'estate-ok' : 'estate-drift'} href={`/ux/jobs/${tunnel.id}`}>{tunnel.text}</a>
          {:else}
            <span class="estate-drift">{tunnel.text}</span>
          {/if}
          {#if tunnel.at}<span class="estate-when">{loopAge(tunnel.at, loadedAt)}</span>{/if}
        </div>
        {#if tunnel.routes.length > 0}
          <ul class="estate-routes">
            {#each tunnel.routes as r, i (`${r}#${i}`)}
              <li>{r}</li>
            {/each}
          </ul>
        {/if}
      {/if}
    </div>

    <div class="estate-section">04 — THE DEV WORKSPACE</div>
    <div class="estate-door">
      {#if door.kind === 'loading'}
        <p class="estate-quiet">Reading the dev door declaration…</p>
      {:else if door.kind === 'failed'}
        <p class="estate-fail">The dev door declaration could not be read: {door.error}</p>
      {:else if door.data.host === null}
        <p class="estate-quiet">This install declares no dev door.</p>
      {:else}
      <p class="estate-hint">
        The workspace answers on <code>{door.data.host}</code>, from anywhere, behind Cloudflare
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
      {/if}
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
  /* The observed half of a machine cell sits under the declared one. */
  .estate-seen { display: block; font-size: 11px; color: var(--static); }
  .estate-seen.estate-drift { color: var(--warn); }
  .estate-legend { color: var(--static); font-size: 12px; max-width: 60ch; margin: 0 0 6px; }
  .estate-alarms { display: flex; flex-direction: column; gap: 6px; font-size: 13px; margin-bottom: 10px; }
  .estate-alarm-row { display: flex; gap: 16px; align-items: baseline; }
  .estate-alarm-none, .estate-alarm-cover { color: var(--static); font-size: 12px; margin: 0; }
  .estate-alarm-link { color: var(--warn); font-size: 12px; }
  .estate-door { display: flex; flex-direction: column; gap: 8px; }
  /* THE INSTANCE VOLUMES, under the machines, in the machines' idiom. */
  .estate-volumes { margin-top: 16px; font-size: 13px; }
  .estate-volumes-none { color: var(--static); font-size: 12px; margin: 0; }
  .estate-volumes-head { display: flex; gap: 16px; align-items: baseline; color: var(--static); font-size: 12px; margin-bottom: 4px; }
  .estate-volume-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .estate-volume-table th {
    text-align: left; font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static); font-weight: 400;
    border-bottom: 1px solid var(--hairline); padding: 4px 12px 4px 0;
  }
  .estate-volume-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline); overflow-wrap: anywhere; }
  /* THE EDGE: one zone line, its records, its Access applications, then
     the tunnel routes — the machines table's own cell idiom. */
  .estate-edge { display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
  .estate-edge-table { width: 100%; border-collapse: collapse; font-size: 13px; margin-bottom: 6px; }
  .estate-edge-table th {
    text-align: left; font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static); font-weight: 400;
    border-bottom: 1px solid var(--hairline); padding: 4px 12px 4px 0;
  }
  .estate-edge-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline); overflow-wrap: anywhere; }
  .estate-edge-note { color: var(--static); font-size: 12px; margin: 0 0 6px; }
  .estate-routes { margin: 0; padding-left: 166px; font-family: var(--font-mono); font-size: 12px; color: var(--static); list-style: none; }
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
