<script lang="ts">
  // A STATION'S RESOURCES VIEW (backlog 87575d69, car C of David's station
  // visuals, 2026-11-19; prototype artboard Resources). The pools the
  // station allocates as a rail of units; each unit drawn where the open
  // claims hold it, gliding there on a claim and back on a return;
  // equipment drawn inside its owning station, never moving; each station
  // saying its pressure; the parent's claim line; the record of moves.
  // Every figure is resources.ts's; this file owns the reads, the stream
  // and the strokes.
  //
  // THE READS: the station tree (the root found by its shape, as the
  // station page finds it), GET /api/pools, GET /api/claims for this
  // station's subtree, GET /api/stations/load, GET /api/claims/waiting
  // for each counted pool, and one GET /api/jobs/{id} for each of the
  // first LINE_PLACED steps in a line, to read the station it declares.
  // EACH READ IS SAID WHEN IT FAILS, and none is drawn as empty.
  //
  // MOTION IS THE RECORD MOVING: GET /api/events/stream, narrowed to the
  // step resource events (jobs.step.reserved, .released,
  // .resource_refused, .resource_lease_expired), triggers a fresh claims
  // and line read; the units that changed holder glide, and the record
  // says which, stamped with the event's own time. A stream that drops
  // is said, and the view reads the claims on a clock until it answers.
  import { onMount, untrack } from 'svelte';
  import { navigate } from '../../../router';
  import { fetchRemote, type Remote } from '../../../data/remote';
  import { parseStationLoad, type StationLoadRow } from '../../marshalling/marshalling';
  import { safeLinkHref } from '@boss/web-kit/links';
  import LocalUtc from '../LocalUtc.svelte';
  import { stationHref } from '../station-page';
  import { claimsPath, jobHref, parseTree, rootIdOf, treeHref, type StationTree } from '../station-tree';
  import {
    GEO, MOVES_MAX, UNPLACED, floorGeometry, lineStationOf, parsePools, parseUnitClaims, parseWaitingLine, resourceFloor,
    resourcesHref, unitMoves,
    type LineRead, type PoolRow, type ResourceFloor, type ResourceMove, type Seats, type Unit, type UnitClaim,
  } from './resources';

  type Props = Readonly<{ station: string }>;
  let { station }: Props = $props();

  const LOADING = { kind: 'loading' } as const;
  /** How many steps of each line are read for the station they declare. */
  const LINE_PLACED = 5;
  /** The claims clock while the stream is down. */
  const POLL_MS = 20_000;
  /** Events landing together are one re-read. */
  const SETTLE_MS = 400;
  /** The step resource events: one substring the stream's `kind` filter matches. */
  const STREAM_KIND = 'jobs.step.re';

  let tree = $state.raw<Remote<StationTree>>(LOADING);
  let pools = $state.raw<Remote<ReadonlyArray<PoolRow>>>(LOADING);
  let claims = $state.raw<Remote<ReadonlyArray<UnitClaim>>>(LOADING);
  let load = $state.raw<Remote<ReadonlyMap<string, StationLoadRow>>>(LOADING);
  let lines = $state.raw<ReadonlyMap<string, LineRead>>(new Map());
  let moves = $state.raw<ReadonlyArray<ResourceMove>>([]);
  let feed = $state<'connecting' | 'live' | 'down'>('connecting');
  let feedWhy = $state('');
  let width = $state(960);

  /** Carried from one floor to the next, never drawn, so not reactive:
   *  the seats the last floor gave each unit (a unit keeps its identity
   *  across reads), that floor (what the record diffs against), and the
   *  time the next re-read's moves are stamped with — the event's own. */
  const carry: { seats: Seats; last: ResourceFloor | null; stamp: string | null } = { seats: new Map(), last: null, stamp: null };

  async function readTree(): Promise<Exclude<Remote<StationTree>, { kind: 'loading' }>> {
    const list = await fetchRemote('/api/stations', (raw: unknown) => raw);
    if (list.kind === 'failed') return list;
    const id = rootIdOf(list.data);
    if (id === null) return { kind: 'failed', error: 'the station registry has no single root: no tree row, or more than one, names no parent' };
    return fetchRemote(`/api/stations?under=${encodeURIComponent(id)}`, parseTree);
  }

  const floor = $derived.by<ResourceFloor | null>(() => {
    if (tree.kind !== 'ready' || pools.kind !== 'ready' || claims.kind !== 'ready' || !tree.data.byName.has(station)) return null;
    return resourceFloor({ tree: tree.data, station, pools: pools.data, claims: claims.data, seats: carry.seats, load, lines });
  });

  // Each new floor: carry its seats forward, and record what moved.
  $effect(() => {
    const f = floor;
    if (f === null) return;
    const before = carry.last;
    if (before !== null && before.station === f.station) {
      const moved = unitMoves(before, f, carry.stamp ?? new Date().toISOString());
      if (moved.length > 0) moves = [...moved, ...untrack(() => moves)].slice(0, MOVES_MAX);
    }
    carry.seats = new Map(f.tokens.map((t) => [t.pool, t.seats] as const));
    carry.last = f;
    carry.stamp = null;
  });

  const geo = $derived(floor === null ? null : floorGeometry(floor, Math.max(GEO.minBox, width)));
  const parent = $derived.by(() => {
    if (tree.kind !== 'ready') return null;
    const p = tree.data.byName.get(station)?.parent ?? null;
    return p === null ? null : (tree.data.nodes.find((n) => n.id === p)?.name ?? null);
  });

  async function readLines(t: StationTree, counted: ReadonlyArray<string>): Promise<void> {
    const read = await Promise.all(counted.map(async (pool) => {
      const r = await fetchRemote(`/api/claims/waiting?pool=${encodeURIComponent(pool)}`, parseWaitingLine);
      if (r.kind !== 'ready') return [pool, r] as const;
      // The first few steps of the line, placed by the station they declare.
      const entries = await Promise.all(r.data.entries.map(async (e, i) => {
        if (i >= LINE_PLACED) return e;
        const job = await fetchRemote(`/api/jobs/${encodeURIComponent(e.jobId)}`, (raw: unknown) => raw);
        return job.kind === 'ready' ? { ...e, station: lineStationOf(t, job.data, e.stepId) } : e;
      }));
      return [pool, { kind: 'ready', data: { ...r.data, entries } }] as const;
    }));
    lines = new Map<string, LineRead>(read);
  }

  async function readClaims(): Promise<void> {
    const r = await fetchRemote(claimsPath(station), parseUnitClaims);
    // A failed re-read keeps the floor drawn from the last good one: the
    // feed line says the view is stale rather than empty the floor.
    if (r.kind === 'ready' || claims.kind !== 'ready') claims = r;
    if (tree.kind === 'ready' && floor !== null) void readLines(tree.data, floor.tokens.map((t) => t.pool));
  }

  let settle: ReturnType<typeof setTimeout> | null = null;
  function moved(at: string): void {
    carry.stamp = at;
    if (settle !== null) clearTimeout(settle);
    settle = setTimeout(() => { settle = null; void readClaims(); }, SETTLE_MS);
  }

  onMount(() => {
    let gone = false;
    void readTree().then((r) => { if (!gone) tree = r; });
    void fetchRemote('/api/pools', parsePools).then((r) => { if (!gone) pools = r; });
    void fetchRemote(claimsPath(station), parseUnitClaims).then((r) => { if (!gone) claims = r; });
    void fetchRemote('/api/stations/load', (raw) => new Map(parseStationLoad(raw).map((r) => [r.station, r] as const))).then((r) => { if (!gone) load = r; });

    let poll: ReturnType<typeof setInterval> | null = null;
    const down = (why: string): void => {
      feed = 'down';
      feedWhy = why;
      if (poll === null) poll = setInterval(() => void readClaims(), POLL_MS);
    };
    let es: EventSource | null = null;
    try {
      es = new EventSource(`/api/events/stream?kind=${encodeURIComponent(STREAM_KIND)}`);
      es.onopen = () => { if (!gone) { feed = 'live'; feedWhy = ''; } };
      es.onmessage = (ev) => {
        if (gone) return;
        let at = new Date().toISOString();
        try {
          const e = JSON.parse(ev.data) as { timestamp?: unknown };
          if (typeof e.timestamp === 'string') at = e.timestamp;
        } catch { /* a frame that is not JSON still says something moved */ }
        moved(at);
      };
      es.addEventListener('failed', (ev) => {
        es?.close();
        down((ev as MessageEvent<string>).data || 'the server ended the stream');
      });
      es.onerror = () => {
        if (gone) return;
        if (es?.readyState === EventSource.CLOSED) down('the stream closed');
        else feed = 'connecting';
      };
    } catch (e) {
      down(e instanceof Error ? e.message : String(e));
    }
    return () => {
      gone = true;
      es?.close();
      if (poll !== null) clearInterval(poll);
      if (settle !== null) clearTimeout(settle);
    };
  });

  // The lines, once the floor knows which pools it counts; again when that set changes.
  const counted = $derived(floor === null ? '' : floor.tokens.map((t) => t.pool).join('\u0000'));
  $effect(() => {
    const which = counted;
    if (which === '' || tree.kind !== 'ready') return;
    void readLines(tree.data, which.split('\u0000'));
  });

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }

  const fmt = (n: number): string => String(Math.round(n * 100) / 100);
  const unitTitle = (u: Unit): string =>
    `${u.pool} unit ${u.index + 1} · ${u.reserved === null ? 'lendable' : `reserved by ${u.reserved}`}` +
    (u.held === null ? ' · idle in the pool'
      : ` · held${u.box === null ? ' where the record cannot place it' : ` at ${u.box}`} by ${u.held.holder ?? 'a holder the record does not name'}` +
        `${u.held.workflowKind === null ? '' : ` (${u.held.workflowKind})`}${u.held.since === null ? '' : ` since ${u.held.since}`}`);
</script>

<section class="resources" data-resources={station}>
  <nav class="head" aria-label="This station's views">
    <h1 class="name">{station}</h1>
    <!-- The Work / Resources toggle: the same station, two views. -->
    <span class="toggle" data-view-toggle>
      <a href={safeLinkHref(stationHref(station))} onclick={(e) => go(e, stationHref(station))} data-view="work">Work</a>
      <span class="on" aria-current="page" data-view="resources">Resources</span>
    </span>
    <span class="grow"></span>
    {#if parent !== null}
      {@const up = resourcesHref(parent)}
      <a class="link" href={safeLinkHref(up)} onclick={(e) => go(e, up)} data-upstream={parent}>↑ {parent}'s resources</a>
    {/if}
    <a class="link" href={safeLinkHref(treeHref(null))} onclick={(e) => go(e, treeHref(null))}>the whole floor</a>
  </nav>

  {#if tree.kind === 'loading' || pools.kind === 'loading' || claims.kind === 'loading'}
    <p role="status" data-resources-loading>Reading the tree, the pools and the claims…</p>
  {:else if tree.kind === 'failed'}
    <p class="failed" data-resources-failed="tree">The station tree could not be read, so nothing is drawn: {tree.error}</p>
  {:else if pools.kind === 'failed'}
    <p class="failed" data-resources-failed="pools">The pools could not be read, so no unit is drawn: {pools.error}</p>
  {:else if claims.kind === 'failed'}
    <p class="failed" data-resources-failed="claims">The claims could not be read, so where each unit is held is unknown and nothing is drawn: {claims.error}</p>
  {:else if floor === null || geo === null}
    <p class="failed" data-resources-missing>The tree read holds no station named <code>{station}</code>.</p>
  {:else}
    <p class="eyebrow">{station}'s pools <span class="sub">· every unit is lendable until reservations are recorded; a unit returns to the pool the moment its claim ends</span></p>

    <div class="pools" data-pools>
      {#each floor.pools as p (p.name)}
        <div class="pool" data-pool={p.name} data-draw={p.draw}>
          <b>{p.name}</b>
          {#if p.draw === 'tokens'}
            {@const t = floor.tokens.find((x) => x.pool === p.name)}
            <span class="fig">{p.size} {p.unit}{p.size === 1 ? '' : 's'} · {p.size - (t?.free ?? 0)} held · <span class:none={t?.free === 0}>{t?.free ?? 0} lendable free</span></span>
            <span class="sub">{p.sizeFrom === 'allocation' ? `${station}'s allocation` : 'the registry\'s capacity'}{(t?.over ?? 0) > 0 ? ` · ${t?.over} more held than the pool holds` : ''}</span>
          {:else if p.draw === 'bar'}
            {#if p.size === null}
              <span class="fig unread" data-unread>{fmt(p.held)} {p.unit} held · size not stated</span>
            {:else}
              <span class="bar" aria-hidden="true"><span style="width:{Math.min(100, (p.held / p.size) * 100)}%"></span></span>
              <span class="fig">{fmt(p.held)} of {fmt(p.size)} {p.unit} held</span>
            {/if}
            {#if p.shares.length > 0}<span class="sub">{p.shares.map((s) => `${s.station} holds ${fmt(s.amount)}`).join(', ')}</span>{/if}
          {:else if p.draw === 'rate'}
            <span class="fig" class:unread={p.rate === null} data-rate>{p.words}</span>
          {:else}
            <span class="fig unread">{p.why}</span>
          {/if}
        </div>
      {/each}
    </div>

    <p class="feed" data-feed={feed}>
      {#if feed === 'live'}live: units move as claims and returns are recorded
      {:else if feed === 'connecting'}connecting to the record…
      {:else}the event stream is down ({feedWhy}), so the claims are read every {POLL_MS / 1000} s instead{/if}
    </p>

    <div class="floor" bind:clientWidth={width} style="height:{geo.height}px" data-floor>
      {#each floor.tokens as t (t.pool)}
        {@const row = geo.rows.get(t.pool)}
        {#if row !== undefined}<span class="rowlabel" style="left:{row.x}px; top:{row.y}px">{t.pool}</span>{/if}
        {#each t.units as u (u.index)}
          {@const s = geo.sockets.get(`${u.pool}#${u.index}`)}
          {#if s !== undefined && u.held !== null}<span class="socket" class:square={floor.tokens.indexOf(t) % 2 === 1} style="left:{s.x}px; top:{s.y}px" aria-hidden="true"></span>{/if}
        {/each}
      {/each}

      {#each floor.boxes as b (b.name)}
        {@const at = geo.boxes.get(b.name)}
        {#if at !== undefined}
          <section class="box p-{b.pressure.state}" style="left:{at.x}px; top:{at.y}px; width:{geo.boxW}px; height:{geo.boxH}px"
            aria-label={b.name} data-box={b.name} data-pressure={b.pressure.state}>
            {#if b.here}
              <span class="bname">{b.name} itself</span>
            {:else}
              {@const to = resourcesHref(b.name)}
              <a class="bname" href={safeLinkHref(to)} onclick={(e) => go(e, to)}>{b.name}</a>
            {/if}
            <span class="pressure" data-pressure-words>{b.pressure.words}</span>
            {#if b.equipment !== null}<span class="baynote">{b.equipment.count} seat{b.equipment.count === 1 ? '' : 's'}, its own equipment</span>{/if}
          </section>
        {/if}
      {/each}
      {#if floor.unplaced > 0}
        {@const at = geo.boxes.get(UNPLACED)}
        {#if at !== undefined}
          <section class="box p-unread" style="left:{at.x}px; top:{at.y}px; width:{geo.boxW}px; height:{geo.boxH}px" data-box="unplaced">
            <span class="bname">held, unplaced</span>
            <span class="pressure">{floor.unplaced} unit{floor.unplaced === 1 ? '' : 's'} held by claims the record cannot place under {station}</span>
          </section>
        {/if}
      {/if}

      {#each floor.boxes as b (b.name)}
        {#if b.equipment !== null}
          {#each Array.from({ length: b.equipment.count }, (_, i) => i) as i (i)}
            {@const at = geo.equipment.get(`${b.name}#${i}`)}
            {#if at !== undefined}
              <span class="seat" class:claimed={i < b.equipment.claimed} style="left:{at.x}px; top:{at.y}px"
                title={`${b.name} seat ${i + 1}: equipment ${b.name} owns outright`} data-seat={`${b.name}#${i}`}></span>
            {/if}
          {/each}
        {/if}
      {/each}

      <!-- The units last, so a moving one glides over every box. -->
      {#each floor.tokens as t, r (t.pool)}
        {#each t.units as u (u.index)}
          {@const at = geo.units.get(`${u.pool}#${u.index}`)}
          {#if at !== undefined}
            <span class="unit" class:square={r % 2 === 1} class:held={u.held !== null} style="left:{at.x}px; top:{at.y}px"
              title={unitTitle(u)} data-unit={`${u.pool}#${u.index}`} data-unit-box={u.held === null ? 'pool' : (u.box ?? 'unplaced')}
              data-lendable={u.reserved === null ? 'true' : 'false'}></span>
          {/if}
        {/each}
      {/each}
    </div>

    <div class="below">
      <section class="panel" aria-labelledby="h-line" data-claim-line>
        <h2 id="h-line">{station}'s claim line <span class="sub">· steps waiting for a lendable unit, in order</span></h2>
        {#each floor.line as l (l.pool)}
          {#if l.state === 'loading'}
            <p class="row muted">{l.pool}: reading the line…</p>
          {:else if l.state === 'failed'}
            <p class="row failed" data-line-failed={l.pool}>{l.pool}: the line could not be read, so whether anything waits is unknown: {l.why}</p>
          {:else if l.entries.length === 0}
            <p class="row muted" data-line-empty={l.pool}>{l.pool}: nothing waits.</p>
          {:else}
            {#each l.entries as e (e.stepId)}
              <p class="row" data-line-entry={e.stepId}>
                <b>#{e.position}</b> {l.pool} ·
                <a href={safeLinkHref(jobHref(e.jobId))}>a packet</a>
                {e.station === null ? '' : `at ${e.station}`}{e.assignee === null ? '' : ` for ${e.assignee}`}
                {#if e.since !== null}· waiting since <LocalUtc at={e.since} />{/if}
              </p>
            {/each}
          {/if}
        {:else}
          <p class="row muted">{station} allocates no counted pool, so it keeps no claim line.</p>
        {/each}
      </section>

      <section class="panel record" aria-labelledby="h-moves" data-moves>
        <h2 id="h-moves">Resource moves <span class="sub">· the record, since this page opened</span></h2>
        {#each moves as m, i (`${m.at}-${i}-${m.text}`)}
          <p class="row" data-move><LocalUtc at={m.at} seconds /> {m.text}{#if m.jobId !== null} · <a href={safeLinkHref(jobHref(m.jobId))}>its packet</a>{/if}</p>
        {:else}
          <p class="row muted" data-moves-none>No unit has moved since this page opened.</p>
        {/each}
      </section>
    </div>

    <p class="legend">
      <span><span class="unit held swatch" aria-hidden="true"></span> unit at work</span>
      <span><span class="unit swatch" aria-hidden="true"></span> unit idle, lendable, in the pool</span>
      <span><span class="seat swatch" aria-hidden="true"></span> seat: equipment, never moves</span>
    </p>
  {/if}
</section>

<style>
  .resources { background: var(--void); color: var(--fog); padding-bottom: var(--s5); }
  .head { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s2) var(--s3); padding: var(--s2) 0; }
  .name { margin: 0; font-size: 20px; font-weight: 800; }
  .toggle { display: inline-flex; border: 1px solid var(--hairline); border-radius: 4px; overflow: hidden; font-size: 13px; }
  .toggle a, .toggle span { padding: 5px 12px; color: var(--fog); text-decoration: none; }
  .toggle a:hover { background: var(--ink-raised); }
  .toggle .on { background: var(--fog); color: var(--void); font-weight: 600; }
  .grow { flex: 1; }
  .link { color: var(--signal); font-size: 12px; text-decoration: none; }
  .link:hover { text-decoration: underline; }
  .failed { color: var(--troubled-ink); font-size: 13px; }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.2em; font-size: 11px; font-weight: 600; margin: var(--s2) 0; }
  .sub { text-transform: none; letter-spacing: 0; font-family: var(--font-body); font-weight: 400; color: var(--static); font-size: 12px; }
  .muted { color: var(--static); }

  .pools { display: flex; flex-wrap: wrap; gap: var(--s2); }
  .pool { display: flex; flex-direction: column; gap: 4px; min-width: 200px; flex: 1 1 200px; max-width: 360px;
    padding: 10px 12px; border: 1px solid var(--hairline); border-radius: 4px; background: var(--ink); font-size: 13px; }
  .fig { font-size: 12px; }
  .fig .none, .unread { color: var(--troubled-ink); font-weight: 600; }
  .bar { display: block; height: 12px; background: var(--ink-raised); border-radius: 3px; overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--static); transition: width 800ms; }
  .feed { font-size: 12px; color: var(--static); margin: var(--s2) 0; }
  .feed[data-feed='down'] { color: var(--troubled-ink); }

  .floor { position: relative; margin-top: var(--s2); }
  .rowlabel { position: absolute; width: 150px; line-height: 36px; font-family: var(--font-mono); font-size: 11px; color: var(--static);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .socket { position: absolute; width: 24px; height: 24px; box-sizing: border-box; border: 2px dashed var(--hairline); border-radius: 12px; }
  .box { position: absolute; box-sizing: border-box; border: 1px solid var(--hairline); border-radius: 6px; background: var(--ink);
    padding: 8px 12px; display: flex; flex-direction: column; gap: 4px; overflow: hidden; }
  .bname { font-weight: 700; font-size: 14px; color: var(--fog); text-decoration: none; }
  a.bname:hover { text-decoration: underline; }
  .pressure { font-size: 11.5px; font-weight: 600; line-height: 1.3; padding: 3px 6px; border-radius: 3px; background: var(--ink-raised); color: var(--static);
    max-height: 2.7em; overflow: hidden; }
  .p-working .pressure { background: var(--signal-wash); color: var(--signal); }
  .p-waiting .pressure { background: var(--warn-wash); color: var(--warn); }
  .p-starved { box-shadow: 0 0 0 2px var(--troubled); }
  .p-starved .pressure { background: var(--err-wash); color: var(--troubled-ink); }
  .p-not-asking .pressure, .p-unread .pressure { background: var(--warn-wash); color: var(--warn); }
  .baynote { font-size: 10.5px; color: var(--static); position: absolute; bottom: 6px; right: 10px; }
  .seat { position: absolute; width: 24px; height: 20px; box-sizing: border-box; border-radius: 3px; border: 2px solid var(--fog); background: var(--ink); }
  .seat.claimed { background: var(--fog); }
  .unit { position: absolute; width: 24px; height: 24px; box-sizing: border-box; border-radius: 12px; border: 2px solid var(--signal); background: var(--ink);
    transition: left 900ms cubic-bezier(.4, 0, .2, 1), top 900ms cubic-bezier(.4, 0, .2, 1), background 300ms; z-index: 2; }
  .unit.square, .socket.square { border-radius: 5px; }
  .unit.held { background: var(--signal); }
  @media (prefers-reduced-motion: reduce) { .unit, .bar span { transition: none; } }

  .below { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: var(--s3); margin-top: var(--s3); }
  @media (max-width: 900px) { .below { grid-template-columns: minmax(0, 1fr); } }
  .panel { border: 1px solid var(--hairline); border-radius: 4px; padding: var(--s2) var(--s3); background: var(--ink); min-width: 0; }
  .panel.record { background: var(--ink-raised); }
  .panel h2 { margin: 0 0 var(--s1); font-size: 13px; font-weight: 700; }
  .row { margin: 4px 0; font-size: 12px; overflow-wrap: anywhere; }
  .row a { color: var(--signal); }
  .legend { display: flex; flex-wrap: wrap; gap: var(--s3); font-size: 11px; color: var(--static); margin-top: var(--s2); }
  .legend > span { display: inline-flex; align-items: center; gap: 6px; }
  .swatch { position: static; display: inline-block; width: 14px; height: 14px; transition: none; }
  .seat.swatch { height: 12px; }
</style>
