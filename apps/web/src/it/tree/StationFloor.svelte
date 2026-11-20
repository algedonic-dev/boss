<script lang="ts">
  // THE STATION FLOOR — the station page's body (backlog dd0eb058, car B
  // of David's station-floor visuals; the Plan and BuildFloor artboards he
  // chose on the prototype canvas, 2026-11-19). Every rule is
  // station-floor.ts's; this file owns the strokes, the reads it adds to
  // the page's, the live events stream and the clock.
  //
  // THE READS IT ADDS: each drawn part's queue (GET
  // /api/stations/{name}/queue — only parts the load says hold a packet,
  // and parts with stations of their own), the flow (GET
  // /api/stations/flow), and the live events stream (GET
  // /api/events/stream). The tree, the load and the claims are the page's,
  // handed in, and `refresh` asks the page to take its claims and load
  // again.
  //
  // MOTION IS THE RECORD MOVING. A frame that names a packet on this floor
  // or a station inside it marks that token and station moving for a
  // moment, appends to the Outbound record when the packet left, and asks
  // for the reads again; a token then stands wherever the re-read record
  // puts it, gliding there because a token is keyed by its packet. Nothing
  // replays: a page opened after a move never shows it, and a stream the
  // browser gave up says so and moves nothing.
  import { navigate } from '../../router';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { parseStationFlow, type StationFlowEnvelope, type StationLoadRow } from '../marshalling/marshalling';
  import type { PageClaims } from './station-page';
  import { subtree, type StationTree } from './station-tree';
  import {
    AGE_BANDS, STILL_BOUND, admitOutbound, floorModel, parseFloorEvent, parseQueue, queuesToRead,
    type Feed, type OutboundRow, type QueueRead,
  } from './station-floor';
  import LocalUtc from './LocalUtc.svelte';

  type Props = Readonly<{
    tree: StationTree;
    station: string;
    load: Remote<ReadonlyMap<string, StationLoadRow>>;
    claims: Remote<PageClaims>;
    refresh: () => void;
  }>;
  let { tree, station, load, claims, refresh }: Props = $props();

  const LOADING = { kind: 'loading' } as const;
  let flow = $state.raw<Remote<StationFlowEnvelope>>(LOADING);
  let queues = $state.raw<ReadonlyMap<string, Remote<QueueRead>>>(new Map());
  let movedJobs = $state.raw<ReadonlyMap<string, number>>(new Map());
  let movedStations = $state.raw<ReadonlyMap<string, number>>(new Map());
  let outbound = $state.raw<ReadonlyArray<OutboundRow>>([]);
  let feed = $state<Feed>('connecting');
  let now = $state(Date.now());
  /** Bumped to take the floor's own reads again after a move. */
  let generation = $state(0);

  // The flow, once per station and again after a move.
  $effect(() => {
    void generation;
    let gone = false;
    void fetchRemote('/api/stations/flow', parseStationFlow).then((r) => { if (!gone) flow = r; });
    return () => { gone = true; };
  });

  // Each part's queue, once the load says which hold a packet. A re-read
  // keeps the last answer drawn until the new one lands, so a token
  // glides rather than blinking out.
  const toRead = $derived(queuesToRead(tree, station, load).join('\n'));
  $effect(() => {
    void generation;
    const names = toRead === '' ? [] : toRead.split('\n');
    let gone = false;
    for (const n of names) {
      void fetchRemote(`/api/stations/${encodeURIComponent(n)}/queue`, parseQueue).then((r) => {
        if (gone) return;
        const next = new Map(queues);
        next.set(n, r);
        queues = next;
      });
    }
    return () => { gone = true; };
  });

  // The clock the stillness reads.
  $effect(() => {
    const t = setInterval(() => { now = Date.now(); }, 1000);
    return () => clearInterval(t);
  });

  const model = $derived(floorModel(tree, station, { load, claims, flow, queues, movedJobs, movedStations }, now));
  const inside = $derived(subtree(tree, station));

  // A burst of frames asks for the reads once, after it settles.
  let pending: ReturnType<typeof setTimeout> | null = null;
  function again(): void {
    if (pending !== null) return;
    pending = setTimeout(() => {
      pending = null;
      refresh();
      generation += 1;
    }, 1500);
  }

  // THE STREAM: this session's moves only.
  $effect(() => {
    const here = station;
    let es: EventSource | null = null;
    feed = 'connecting';
    try {
      es = new EventSource('/api/events/stream');
    } catch {
      feed = 'down';
      return;
    }
    es.onopen = () => { feed = 'live'; };
    es.addEventListener('failed', () => { feed = 'down'; es?.close(); });
    es.onmessage = (ev: MessageEvent<string>) => {
      const e = parseFloorEvent(ev.data);
      if (e === null) return;
      const onFloor = model.onFloor;
      const atStation = e.station !== null && inside.has(e.station);
      const ours = (e.job !== null && onFloor.has(e.job)) || atStation;
      if (!ours) return;
      const t = Date.now();
      if (e.job !== null) movedJobs = new Map([...movedJobs, [e.job, t]]);
      if (atStation) movedStations = new Map([...movedStations, [e.station!, t]]);
      outbound = admitOutbound(outbound, e, tree, here, onFloor);
      again();
    };
    es.onerror = () => {
      // A stream the browser is retrying stays as it was; only one it
      // gave up is down.
      if (es !== null && es.readyState === EventSource.CLOSED) feed = 'down';
    };
    return () => {
      es?.close();
      if (pending !== null) clearTimeout(pending);
      pending = null;
    };
  });

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }

  const bandClass = ['a1', 'a2', 'a3', 'a4'] as const;
  const fmtDay = (h: number) => (h >= 48 ? `${Math.floor(h / 24)} d` : `${h} h`);
</script>

<section class="floor-wrap" id="floor" data-band="floor" data-floor={station} aria-label={`${station}'s floor`}>
  <div class="floor-head">
    <h2 class="eyebrow">Floor <span class="sub">{station} · a system of systems</span></h2>
    <span class="feed" data-feed={feed}>
      {#if feed === 'live'}live: moves as the record moves{:else if feed === 'connecting'}connecting to the events stream…{:else}the events stream is down — nothing moves until the page is opened again{/if}
    </span>
  </div>
  {#if feed === 'down'}
    <p class="load-failed" data-feed-down>The live events stream (GET /api/events/stream, read by operators and auditors) was given up by the browser, so no token moves and no departure is recorded on this page.</p>
  {/if}
  {#each model.notes as n (n)}<p class="note" data-floor-note>{n}</p>{/each}

  <div class="scroller">
    <div class="ground" style="width:{model.layout.width}px; height:{model.layout.height}px">
      <svg class="routes" width={model.layout.width} height={model.layout.height} aria-hidden="true">
        <defs>
          <marker id="floor-ah" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" class="ah" /></marker>
          <marker id="floor-ah-hot" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" class="ah hot" /></marker>
        </defs>
        {#each model.routes as r (r.key)}
          <path d={r.d} class="route" class:dashed={r.dashed} class:unread={r.unread} class:hot={r.hot}
            marker-end={r.hot ? 'url(#floor-ah-hot)' : 'url(#floor-ah)'} data-route={r.key} data-route-state={r.unread ? 'unread' : r.dashed ? 'none' : 'crossed'}><title>{r.title}</title></path>
        {/each}
      </svg>
      {#each model.routes as r (r.key)}
        {#if r.label !== null}<span class="rlabel" style="left:{r.lx}px; top:{r.ly}px">{r.label}</span>{/if}
      {/each}

      <!-- THE ADMISSION MACHINE: the station's one edge in. -->
      <section class="machine" data-admission={station} style="left:{model.layout.admission.x}px; top:{model.layout.admission.y}px; width:{model.layout.admission.w}px; height:{model.layout.admission.h}px">
        <b>admission</b>
        <span class="dim">{model.admission.words}</span>
        <span class="dim" data-still>{model.admission.still.words}</span>
      </section>

      {#each model.parts as p (p.name)}
        <section class="station" class:equipment={p.equipment} class:moving={p.moving} class:past={p.holding && p.still.past}
          data-floor-station={p.name} data-placed={p.box.placed}
          style="left:{p.box.x}px; top:{p.box.y}px; width:{p.box.w}px; height:{p.box.h}px">
          <div class="srow">
            {#if p.self}
              <b class="sname">{p.name}</b>
            {:else}
              <a class="sname" href={safeLinkHref(p.href)} onclick={(e) => go(e, p.href)} title={`Open ${p.title}`}>{p.name}</a>
            {/if}
            <span class="dim" data-flow>{p.flow}</span>
          </div>
          <div class="srow small">
            <span class="still-for" data-still data-still-past={p.holding && p.still.past ? 'true' : undefined}>{p.moving ? 'moving now' : p.still.words}</span>
            <span class="dim" data-queue-state={p.queue.kind}>
              {#if p.queue.kind === 'ready'}{p.queue.total} admitted{:else if p.queue.kind === 'empty'}empty{:else if p.queue.kind === 'loading'}reading…{:else}queue unread{/if}
            </span>
          </div>
          {#if p.queue.kind === 'unread'}<p class="unread-why" data-queue-unread>{p.queue.why}</p>{/if}
          {#each p.slots as s, i (i)}
            <span class="slot" aria-hidden="true" style="left:{s.x - p.box.x}px; top:{s.y - p.box.y}px"></span>
          {/each}
          {#if p.pile !== null}
            {@const pile = p.pile}
            <div class="pile" role="img" data-pile={pile.total}
              aria-label={`${pile.words}: ${pile.bands.map((n, i) => `${n} ${AGE_BANDS[i]}`).join(', ')}`}
              style="top:{p.pileTop - p.box.y}px; height:{pile.thickness}px">
              {#each pile.bands as n, i (i)}
                {#if n > 0}<span class={bandClass[i]} style="flex:{n} 1 0px" title={`${n} ${AGE_BANDS[i]}`} data-age-band={i}></span>{/if}
              {/each}
              {#if pile.unaged > 0}<span class="aun" style="flex:{pile.unaged} 1 0px" title={`${pile.unaged} of unread age`}></span>{/if}
            </div>
            <p class="pile-words" style="top:{p.pileTop - p.box.y + pile.thickness + 4}px">{pile.words}</p>
          {/if}
          {#if p.moreWorking > 0}<p class="more" data-more-working>+{p.moreWorking} more working</p>{/if}
        </section>
      {/each}

      <!-- OUTBOUND: a record of what left and where it went, never a place to wait. -->
      <section class="record" data-outbound={station} style="left:{model.layout.outbound.x}px; top:{model.layout.outbound.y}px; width:{model.layout.outbound.w}px; height:{model.layout.outbound.h}px">
        <div class="srow"><b>outbound</b><span class="dim">record</span></div>
        <p class="dim small">what left {station}, and where it went — this session</p>
        {#each outbound as o (o.key)}
          <p class="ledger" data-outbound-row={o.key}><LocalUtc at={o.at} /> <a href={safeLinkHref(o.href)}>{o.title}</a>: {o.words}</p>
        {:else}
          <p class="dim small" data-outbound-empty>{feed === 'down' ? 'Not recorded: the events stream is down.' : 'Nothing has left since this page opened.'}</p>
        {/each}
      </section>

      {#each model.peers as peer (peer.name)}
        <section class="peer" data-peer={peer.name} style="left:{peer.box.x}px; top:{peer.box.y}px; width:{peer.box.w}px; height:{peer.box.h}px">
          <a class="sname" href={safeLinkHref(peer.href)} onclick={(e) => go(e, peer.href)}>{peer.name}</a>
          <span class="dim small">peer station: its own admission routes it inside</span>
        </section>
      {/each}

      {#each model.tokens as t (t.key)}
        <a class="token {t.place}" class:moving={t.moving} class:past={t.still.past && !t.moving}
          href={safeLinkHref(t.href)} title={`${t.title} · ${t.still.words}`} data-token={t.key} data-token-place={t.place}
          data-token-past={t.still.past ? 'true' : undefined}
          style="left:{t.x}px; top:{t.y}px">
          <span class="tl">{t.label}</span><span class="tm">{t.moving ? 'moving' : t.meta}</span>
        </a>
      {/each}
    </div>
  </div>

  <p class="legend" data-floor-legend>
    <span><i class="k waiting"></i>in an admission queue</span>
    <span><i class="k working"></i>in a working slot</span>
    <span><i class="k moving"></i>moving now</span>
    <span><i class="k past"></i>still past its bound: waiting over {fmtDay(STILL_BOUND.waiting / 3_600_000)}, held or quiet over {fmtDay(STILL_BOUND.working / 3_600_000)}</span>
    <span>behind the head, by age:
      {#each AGE_BANDS as b, i (b)}<i class="k {bandClass[i]}"></i>{b} {/each}· thicker with depth</span>
    <span>A dashed route: its source recorded nothing leaving in the flow read's window. No read counts a crossing per route, so none is drawn.</span>
  </p>
</section>

<style>
  .floor-wrap { margin-top: var(--s3); border-top: 1px solid var(--hairline); padding-top: var(--s3); }
  .floor-head { display: flex; align-items: baseline; justify-content: space-between; flex-wrap: wrap; gap: var(--s2); }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.24em; font-size: 11px; color: var(--fog); margin: 0 0 var(--s2); font-weight: 600; }
  .eyebrow .sub { text-transform: none; letter-spacing: 0; font-family: var(--font-body); font-weight: 400; color: var(--static); margin-left: var(--s2); }
  .feed { font-size: 12px; color: var(--static); }
  .feed[data-feed='live'] { color: var(--floor-move-ink); }
  .feed[data-feed='down'] { color: var(--troubled-ink); }
  .note { font-size: 12px; color: var(--static); font-style: italic; margin: var(--s1) 0; }

  /* The floor scrolls inside itself, so the page never scrolls sideways. */
  .scroller { overflow-x: auto; max-width: 100%; border: 2px solid var(--floor-rule); border-radius: 14px; background: var(--floor-ground); }
  .ground { position: relative; }
  .routes { position: absolute; left: 0; top: 0; }
  .route { fill: none; stroke: var(--static); stroke-width: 2; }
  .route.dashed { stroke-dasharray: 5 5; }
  .route.unread { stroke: var(--hairline); stroke-dasharray: 2 4; }
  .route.hot { stroke: var(--floor-move); stroke-width: 4; }
  .ah { fill: var(--static); }
  .ah.hot { fill: var(--floor-move); }
  .rlabel { position: absolute; transform: translate(-50%, -150%); padding: 1px 6px; border-radius: 4px; background: var(--floor-ground); font-size: 11px; color: var(--static); font-weight: 600; white-space: nowrap; }

  .machine, .station, .record, .peer { position: absolute; box-sizing: border-box; border-radius: 12px; }
  .machine { background: var(--floor-machine); color: var(--on-floor-machine); border-radius: 10px; padding: 10px; display: flex; flex-direction: column; gap: 6px; font-size: 12px; }
  .machine .dim { color: var(--on-floor-machine); opacity: 0.85; }
  .station { background: var(--void); border: 1px solid var(--hairline); transition: box-shadow 300ms; }
  .station.equipment { border-color: var(--static); }
  .station.moving { box-shadow: 0 0 0 3px var(--floor-move); }
  .station.past { box-shadow: 0 0 0 2px var(--floor-still); }
  .srow { display: flex; align-items: baseline; justify-content: space-between; gap: 6px; padding: 8px 10px 0; }
  .srow.small, .small { font-size: 11px; }
  .srow.small { padding-top: 2px; }
  .sname { font-size: 13px; font-weight: 700; color: var(--fog); }
  a.sname { color: var(--signal); text-decoration: none; }
  a.sname:hover { text-decoration: underline; }
  .dim { color: var(--static); font-size: 11px; }
  .still-for { font-weight: 600; color: var(--static); }
  .station.moving .still-for { color: var(--floor-move-ink); }
  .still-for[data-still-past='true'] { color: var(--floor-still-ink); }
  .unread-why { margin: 4px 10px; font-size: 11px; color: var(--troubled-ink); }
  .slot { position: absolute; width: 104px; height: 24px; box-sizing: border-box; border: 1px dashed var(--floor-rule); border-radius: 6px; }
  .pile { position: absolute; left: 8px; right: 8px; display: flex; gap: 1px; border-radius: 4px; overflow: hidden; }
  .a1 { background: var(--floor-age-1); }
  .a2 { background: var(--floor-age-2); }
  .a3 { background: var(--floor-age-3); }
  .a4 { background: var(--floor-age-4); }
  .aun { background: var(--floor-age-unread); }
  .pile-words { position: absolute; left: 8px; right: 8px; margin: 0; font-size: 10px; line-height: 1.3; color: var(--static); }
  .more { position: absolute; right: 10px; bottom: 2px; margin: 0; font-size: 10px; color: var(--static); }

  .record { background: var(--ink-raised); border: 1px solid var(--floor-rule); overflow: hidden; }
  .record .small { margin: 2px 10px 0; }
  .ledger { margin: 6px 10px 0; padding-top: 6px; border-top: 1px solid var(--hairline); font-size: 11px; line-height: 1.35; color: var(--fog); }
  .peer { border: 2px dashed var(--static); background: var(--void); padding: 8px 10px; display: flex; flex-direction: column; gap: 4px; }

  .token { position: absolute; z-index: 2; width: 104px; height: 24px; box-sizing: border-box; display: flex; align-items: center; justify-content: space-between; gap: 4px;
    padding: 0 6px; border-radius: 6px; background: var(--void); box-shadow: 0 0 0 1px var(--static); color: var(--fog); font-size: 11px; font-weight: 500; text-decoration: none;
    transition: left 800ms cubic-bezier(.4, 0, .2, 1), top 800ms cubic-bezier(.4, 0, .2, 1), background 300ms; }
  .token.working { background: var(--floor-work-wash); box-shadow: 0 0 0 1px var(--floor-work); }
  .token.moving { background: var(--floor-move-wash); box-shadow: 0 0 0 1px var(--floor-move); }
  .token.past { background: var(--floor-still-wash); box-shadow: 0 0 0 2px var(--floor-still); }
  .token.past .tm { color: var(--floor-still-ink); font-weight: 700; }
  .tl { flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .tm { flex: 0 1 auto; max-width: 55%; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 10px; color: var(--static); }
  @media (prefers-reduced-motion: reduce) { .token { transition: none; } }

  .legend { display: flex; flex-wrap: wrap; gap: 6px 16px; font-size: 12px; color: var(--static); margin-top: var(--s2); }
  .legend span { display: inline-flex; align-items: center; gap: 4px; flex-wrap: wrap; }
  .k { display: inline-block; width: 14px; height: 10px; border-radius: 3px; }
  .k.waiting { background: var(--void); box-shadow: 0 0 0 1px var(--static); }
  .k.working { background: var(--floor-work-wash); box-shadow: 0 0 0 1px var(--floor-work); }
  .k.moving { background: var(--floor-move-wash); box-shadow: 0 0 0 1px var(--floor-move); }
  .k.past { background: var(--floor-still-wash); box-shadow: 0 0 0 2px var(--floor-still); }
</style>
