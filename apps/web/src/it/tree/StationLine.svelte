<script lang="ts">
  // THE STATION LINE — the station page's body (design 08d1530e car B,
  // approved by David 2026-11-12 with Q1 "interiors inline on the line";
  // exhibit E2; backlog 9d0b0944). It replaces the motion band
  // (StationMotion.svelte): the busiest owned protocol's line, each stop a
  // station box with its admission column, its seats (the row's capacity)
  // as cells holding named packets with their dwell, and every path
  // between them.
  // Every rule is station-line.ts's; this owns the reads, the one
  // EventSource and the strokes.
  //
  // REAL, CURRENT ACTIVITY ONLY (David, 2026-11-08 00:50Z). A packet moves
  // on this line only when GET /api/yard/moves/stream names it while the
  // page is open: the page reads that packet and the claims again, and the
  // packet stands where the record now puts it, marked for a moment. A
  // move recorded before the page opened is never drawn, and nothing
  // replays. A stream the browser gave up is STALE and says why.
  import { onMount } from 'svelte';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { parseMove, parseResync, type Move } from './move-wire';
  import type { StationTree } from './station-tree';
  import { HOLDER_MARK, parseHeldClaims, type HeldClaim } from './station-page';
  import LocalUtc from './LocalUtc.svelte';
  import {
    admitMove, deltaWords, feedState, jobHref, resourceDelta, stationProtocols,
    type Feed, type Moving, type SessionMove,
  } from './station-motion';
  import {
    BOX, boxGeometry, defaultProtocol, parseKindCounts, parseLinePacket, parseLinePackets, parseLineProtocols, protocolChips, stationLine,
    type KindCount, type LinePacket, type LinePacketsRead, type LineProtocols,
  } from './station-line';

  type Props = Readonly<{
    tree: StationTree;
    station: string;
    /** What moved with a packet, while it is marked; the page's band may show it. */
    onMoving?: (m: Moving | null) => void;
  }>;
  let { tree, station, onMoving = () => {} }: Props = $props();

  /** How long a packet the stream just moved stays marked. */
  const ARRIVED_MS = 2400;
  const PACKETS_LIMIT = 200;
  const GAP = 14;
  const LOADING = { kind: 'loading' } as const;

  let protocols = $state.raw<Remote<LineProtocols>>(LOADING);
  let counts = $state.raw<Remote<ReadonlyMap<string, KindCount>>>(LOADING);
  let claims = $state.raw<Remote<ReadonlyArray<HeldClaim>>>(LOADING);
  let packets = $state.raw<Remote<LinePacketsRead>>(LOADING);
  let picked = $state<string | null>(null);
  let now = $state(Date.now());
  let feed = $state<Feed>('connecting');
  let feedWhy = $state('');
  let moves = $state.raw<ReadonlyArray<SessionMove>>([]);
  let arrived = $state<string | null>(null);
  const opened = new Date().toISOString();

  const choices = $derived(protocols.kind === 'ready' ? stationProtocols(tree, station, protocols.data.protocols) : []);
  const countMap = $derived(counts.kind === 'ready' ? counts.data : null);
  /** The chosen kind: the one picked, else the busiest — chosen only once
   *  the counts have answered, so the line is not read twice. */
  const chosen = $derived(
    picked !== null && choices.some((c) => c.kind === picked) ? picked
    : counts.kind === 'loading' ? null
    : defaultProtocol(choices, countMap),
  );
  const chips = $derived(protocolChips(choices, countMap, chosen));
  const protocol = $derived(protocols.kind === 'ready' && chosen !== null ? (protocols.data.protocols.find((p) => p.kind === chosen) ?? null) : null);
  const model = $derived(protocol === null || packets.kind !== 'ready' || protocols.kind !== 'ready' ? null : stationLine({
    tree, station, protocol,
    version: protocols.data.versions.get(protocol.kind) ?? null,
    packets: packets.data,
    claims: claims.kind === 'ready' ? claims.data : null,
    now,
  }));
  const boxes = $derived.by(() => {
    if (model === null) return [];
    let x = 0;
    return model.stops.map((stop) => {
      const geo = stop.station === null ? null : boxGeometry(stop);
      const width = geo?.width ?? BOX.noStation;
      const at = x;
      x += width + GAP;
      return { stop, geo, width, x: at };
    });
  });
  const fs = $derived(feedState(feed, feedWhy));
  const shown = $derived(chosen === null ? [] : moves.filter((m) => m.kind === chosen));

  onMount(() => {
    let gone = false;
    void fetchRemote('/api/workflows', parseLineProtocols).then((r) => { if (!gone) protocols = r; });
    void fetchRemote('/api/jobs/kinds', parseKindCounts).then((r) => { if (!gone) counts = r; });
    // Every open claim, not the page's subtree: a claim standing outside
    // the tree still admits its step (backlog d05e29a7 narrowed the page's).
    void fetchRemote('/api/claims', parseHeldClaims).then((r) => { if (!gone) claims = r; });
    const tick = setInterval(() => { now = Date.now(); }, 30_000);
    return () => { gone = true; clearInterval(tick); };
  });

  // The chosen protocol's open packets, read again whenever another is chosen.
  $effect(() => {
    const kind = chosen;
    if (kind === null) return;
    let gone = false;
    packets = LOADING;
    void fetchRemote(`/api/jobs?kind=${encodeURIComponent(kind)}&status=open&limit=${PACKETS_LIMIT}`, parseLinePackets)
      .then((r) => { if (!gone) packets = r; });
    return () => { gone = true; };
  });

  function replace(after: LinePacket): void {
    if (packets.kind !== 'ready') return;
    const rest = packets.data.packets.filter((p) => p.id !== after.id);
    const known = rest.length !== packets.data.packets.length;
    const total = packets.data.total + (after.open ? (known ? 0 : 1) : (known ? -1 : 0));
    packets = { kind: 'ready', data: { packets: after.open ? [...rest, after] : rest, total } };
  }

  const stepOf = (p: LinePacket | null): string | null =>
    p === null ? null : (p.steps.find((s) => s.status === 'active')?.slug ?? p.steps.find((s) => s.status === 'ready')?.slug ?? null);

  let marks = 0;
  /** One recorded move of the chosen protocol, as it arrives. */
  async function take(mv: Move): Promise<void> {
    if (chosen === null || mv.kind !== chosen) return;
    const before = packets.kind === 'ready' ? (packets.data.packets.find((p) => p.id === mv.packet) ?? null) : null;
    const claimsBefore = claims.kind === 'ready' ? claims.data : null;
    const read = await fetchRemote(`/api/jobs/${encodeURIComponent(mv.packet)}`, parseLinePacket);
    const after = read.kind === 'ready' ? read.data : null;
    const again = await fetchRemote('/api/claims', parseHeldClaims);
    if (again.kind === 'ready') claims = again;
    const claimsAfter = again.kind === 'ready' ? again.data : null;
    const delta = claimsBefore === null || claimsAfter === null ? null : resourceDelta(claimsBefore, claimsAfter, mv.packet);
    const words = claimsBefore === null || claimsAfter === null ? 'the claims could not be read' : deltaWords(delta);
    const from = stepOf(before);
    const to = after === null ? from : stepOf(after);
    moves = admitMove(moves, {
      seq: mv.seq,
      at: mv.at,
      packet: mv.packet,
      label: mv.label,
      href: jobHref(mv.packet),
      kind: mv.kind,
      route: read.kind === 'failed' ? `the packet could not be read: ${read.error}` : (from !== null && from === to ? `still at ${from}` : `${from ?? 'filed'} › ${to ?? 'off the line'}`),
      regions: `${mv.from ?? 'filed'} › ${mv.to ?? 'off the map'}`,
      delta: words,
    });
    if (after === null) return;
    replace(after);
    if (!fs.moves || document.visibilityState !== 'visible') return;
    marks += 1;
    const key = marks;
    arrived = mv.packet;
    onMoving({ label: mv.label, pools: (delta ?? []).map((d) => d.pool), words });
    setTimeout(() => {
      if (marks !== key) return;
      arrived = null;
      onMoving(null);
    }, ARRIVED_MS);
  }

  onMount(() => {
    let seq: number | null = null;
    const es = new EventSource('/api/yard/moves/stream');
    es.addEventListener('open', () => { feed = 'live'; });
    es.addEventListener('resync', (e) => {
      // The record's place is the baseline: nothing moved.
      const r = parseResync((e as MessageEvent<string>).data);
      if (r !== null) seq = r.seq;
      feed = 'live';
    });
    es.addEventListener('move', (e) => {
      const mv = parseMove((e as MessageEvent<string>).data);
      if (mv === null || (seq !== null && mv.seq <= seq)) return;
      seq = mv.seq;
      void take(mv);
    });
    es.addEventListener('error', () => {
      if (es.readyState !== EventSource.CLOSED) return;
      feed = 'down';
      feedWhy = 'the browser gave the stream up';
    });
    return () => es.close();
  });

  const plural = (n: number, one: string, many = `${one}s`): string => `${n} ${n === 1 ? one : many}`;
</script>

<section class="line-band" data-band="line" aria-labelledby="h-line">
  <h2 id="h-line" class="eyebrow">How {station} moves its work
    <span class="sub">{model === null ? 'each station takes packets in from its admission queue, routes them through its parts, and sends them on' : `${model.kind}${model.version === null ? '' : ` v${model.version}`}: each station takes packets in from its admission queue, routes them through its parts, and sends them on`}</span>
  </h2>
  {#if protocols.kind === 'loading' || (counts.kind === 'loading' && picked === null)}
    <p role="status">reading the protocols…</p>
  {:else if protocols.kind === 'failed'}
    <p class="load-failed" data-line-failed="workflows">the protocols could not be read, so no line is drawn: {protocols.error}</p>
  {:else if chosen === null}
    <p class="none" data-no-protocols>No protocol names {station} or a station inside it as its owner, so there is no line to draw.</p>
  {:else}
    <nav class="choose" aria-label="Protocol" data-protocol-chips>
      {#each chips.chips as c (c.kind)}
        <button type="button" class="chip" class:on={c.kind === chosen} aria-pressed={c.kind === chosen} data-chip={c.kind}
          title={c.owner === station ? `${c.kind}, owned by ${station}` : `${c.kind}, owned by ${c.owner}`}
          onclick={() => (picked = c.kind)}>{c.kind}{#if c.open !== null}<span class="num">{c.open}</span>{/if}</button>
      {/each}
      {#if chips.none > 0}<span class="muted" data-chips-none>{chips.none} more, none open</span>{/if}
      <span class="grow"></span>
      <span class="plate {fs.plate}" data-feed={fs.plate}>{fs.word}</span>
    </nav>
    {#if counts.kind === 'failed'}<p class="load-failed" data-line-failed="kinds">the open counts could not be read, so the line opens on the first protocol and the chips carry no number: {counts.error}</p>{/if}
    {#if claims.kind === 'failed'}<p class="load-failed" data-line-failed="claims">the claims could not be read, so a claimed step is not told apart from a waiting one: {claims.error}</p>{/if}
    {#if fs.why !== null}<p class="why" data-feed-why>{fs.why}</p>{/if}

    {#if packets.kind === 'loading'}
      <p role="status">reading the open {chosen} packets…</p>
    {:else if packets.kind === 'failed'}
      <p class="load-failed" data-line-failed="packets">the open packets could not be read, so no packet is drawn: {packets.error}</p>
    {:else if model !== null}
      <div class="scroller">
        <div class="stage" class:hollow={fs.plate === 'stale'} data-line={model.kind}
          role="group" aria-label={`${model.kind} line: ${model.stops.map((s) => `${s.slug} at ${s.station ?? 'no station'}, ${s.in.count} not admitted, ${s.cells.filter((c) => !c.free).length + s.more} held`).join('; ')}`}>
          {#each boxes as b, i (b.stop.slug)}
            {@const s = b.stop}
            <div class="col" class:last={i === boxes.length - 1} style="width:{b.width}px; --gap:{GAP}px" data-stop={s.slug} data-station={s.station ?? ''}>
              <span class="dot" class:trb={s.idle} class:none={s.station === null} aria-hidden="true"></span>
              <span class="stoplbl">{s.slug}</span>
              {#if s.station === null || b.geo === null}
                <div class="st nostation">
                  <header><b>{s.handoff !== null ? 'hand-off' : 'not placed'}</b></header>
                  <p class="nonote" data-stop-note>{s.note}</p>
                  {#if s.tray > 0}<p class="nonote" data-stop-tray><b class="num">{s.tray}</b> here, counted in the tray below</p>{/if}
                </div>
              {:else}
                {@const g = b.geo}
                <div class="st" class:trb={s.idle} data-box={s.station}>
                  <div class="inner" style="height:{BOX.h}px">
                    <header>
                      <b>{s.station}</b>
                      <span class="k">{[s.machines.map((m) => m.replace(/^automation:/, '')).join(', '), s.seats === null ? null : plural(s.seats, 'seat')].filter((x) => x !== null && x !== '').join(' · ')}</span>
                      {#if s.idle}<span class="plate troubled" data-idle>idle with a queue</span>{/if}
                    </header>
                    <svg width={g.width} height={BOX.h} aria-hidden="true">
                      {#each g.bundles as bn, k (k)}
                        <path class="bundle" d={bn.d} />
                        {#if k === 0}<text class="bl" x={bn.x - 3} y={bn.y + 4}>{bn.count}</text>{/if}
                      {/each}
                      {#each g.paths as p, k (k)}<path class="p" class:idle={p.idle} d={p.d} data-path={p.part} />{/each}
                      <circle class="node" class:idle={s.cells.every((c) => c.free)} cx={g.receipt.x} cy={g.receipt.y} r="5" />
                    </svg>
                    <div class="adm" data-in={s.in.count} title="arrived, not admitted: a ready step with no open claim">
                      <span class="h">in</span>
                      {#each s.in.tags as t (t.id)}
                        <a class="tg" class:arrived={arrived === t.id} href={safeLinkHref(t.href)} title={t.title} data-in-tag={t.id}>{t.title}</a>
                      {/each}
                      <span class="n num" class:hot={s.idle}>{s.in.count}</span>
                      {#if s.in.oldest !== null}<span class="a num" title="oldest, since the step before it completed">{s.in.oldest}</span>{/if}
                    </div>
                    {#if s.queue !== null && g.queueNode !== null}
                      {@const q = s.queue}
                      <div class="bq" style="left:{g.queueNode.x - 22}px; top:{g.queueNode.y - 30}px" data-machine-queue={q.count}
                        title={q.tags.length === 0 ? 'nothing waits for a seat' : `waiting for a seat: ${q.tags.map((t) => t.title).join(', ')}`}>
                        <span class="lab">machine queue</span>
                        <span class="c" class:on={q.count > 0}></span>
                        <span class="n num">{q.count}{q.oldest === null ? '' : ` · ${q.oldest}`}</span>
                      </div>
                    {/if}
                    {#each g.cells as c (c.cell.key)}
                      {@const cell = c.cell}
                      {#if cell.packet !== null}
                        <a class="pt" class:arrived={arrived === cell.packet.id} style="left:{c.x}px; top:{c.y}px; width:{BOX.cellW}px" href={safeLinkHref(cell.packet.href)}
                          data-cell={cell.packet.id} data-cell-label={cell.label}
                          title={`${cell.packet.title} — ${cell.holder ?? cell.label}${cell.dwell === null ? '' : `, ${cell.dwell}`}`}>
                          <span class="c on"></span>
                          {#if cell.mark !== null}<span class="mk" data-mark>{cell.mark}</span>{/if}
                          <span class="t">{cell.packet.title}</span>
                          {#if cell.dwell !== null}<span class="d num">{cell.dwell}</span>{/if}
                        </a>
                        {#if g.cells.length <= 4 && cell.holder === null}<span class="lab" style="left:{c.x}px; top:{c.y - 12}px">{cell.label}</span>{/if}
                      {:else}
                        <span class="pt free" style="left:{c.x}px; top:{c.y}px; width:{BOX.cellW}px" data-cell="" data-cell-label={cell.label}>
                          <span class="c"></span><span class="t">{cell.label}</span>
                        </span>
                      {/if}
                    {/each}
                    {#if s.more > 0}<span class="lab more" style="left:{g.cells[0]?.x ?? 0}px; top:{BOX.h - 22}px" data-cells-more>+{s.more} more held</span>{/if}
                    <span class="lab" style="left:{g.receipt.x - 16}px; top:{g.receipt.y + 26}px">receipt</span>
                  </div>
                  {#if s.notes.length > 0 || s.note !== null}
                    <ul class="foot" data-box-notes>
                      {#if s.note !== null}<li>{s.note}</li>{/if}
                      {#each s.notes as n (n)}<li>{n}</li>{/each}
                    </ul>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      </div>
      <p class="legend">
        <span><i class="dash"></i>arrived, not admitted</span>
        <span><i class="on"></i>seat held</span>
        <span><i class="bun"></i>more than 4 parallel paths, bundled with the count</span>
        <span>{HOLDER_MARK.person} person · {HOLDER_MARK.robot} robot · {HOLDER_MARK.automaton} automaton, from the claim</span>
        <span>a packet moves only when a recorded move names it</span>
      </p>

      <section class="tray-band" aria-labelledby="h-tray" data-tray>
        <h3 id="h-tray">Not on this line <span class="sub">counted here, never placed at a station by guessing</span></h3>
        {#if model.tray.length === 0 && model.read.unread === 0}
          <p class="none" data-tray-empty>Every open {model.kind} packet read stands at a station its step names.</p>
        {:else}
          <div class="tray">
            {#each model.tray as r (`${r.reason}:${r.step}:${r.version}`)}
              <b class="num">{r.count}</b>
              <span data-tray-row={r.step ?? ''}>{r.step === null ? '' : `at ${r.step}`}{r.version === null ? '' : ` · v${r.version}`}: {r.reason}.
                {#each r.sample as t (t.id)}<a href={safeLinkHref(t.href)} title={t.title}>{t.title}</a>{' '}{/each}</span>
            {/each}
            {#if model.read.unread > 0}
              <b class="num">{model.read.unread}</b><span data-tray-unread>more open {model.kind} packets were not read: this page reads the first {model.read.shown} of {model.read.total}.</span>
            {/if}
          </div>
        {/if}
      </section>

      <div class="moves" data-moves>
        <h3>Moves <span class="sub">this page's, newest first, from the record; nothing replays</span></h3>
        {#each shown as m (m.seq)}
          <div class="move" data-move={m.seq}>
            <span class="at num"><LocalUtc at={m.at} seconds /></span>
            <a href={safeLinkHref(m.href)} class="t">{m.label}</a>
            <span class="route" data-move-route>{m.route}</span>
            {#if m.delta !== null}<span class="muted" data-move-delta>{m.delta}</span>{/if}
          </div>
        {:else}
          <p class="none" data-no-moves>No {model.kind} packet has moved since this page opened at <LocalUtc at={opened} />. A move recorded before then is not drawn.</p>
        {/each}
      </div>
    {/if}
  {/if}
</section>

<style>
  /* Enamel on the Transit palette (design a4df741a), drawn as exhibit E2:
     night-ink frames, 4px corners, the department's line as the track.
     The red rail of a failed read is styles.css's `.load-failed`. */
  .line-band { margin-top: var(--s4); border-top: 1px solid var(--hairline); padding-top: var(--s3); --line: var(--map-line-it, var(--fog)); }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.24em; font-size: 11px; color: var(--fog); margin: 0 0 var(--s2); font-weight: 600; }
  .eyebrow .sub { text-transform: none; letter-spacing: 0; font-family: var(--font-body); font-weight: 400; color: var(--static); margin-left: var(--s2); }
  .muted, .sub { color: var(--static); font-size: 12px; }
  .none { font-size: 12px; color: var(--static); margin: var(--s1) 0; }
  .num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .grow { flex: 1; }
  .why { font-size: 11.5px; color: var(--troubled-ink); margin: 2px 0; }

  .choose { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin-bottom: var(--s2); }
  .chip { font: inherit; font-size: 12px; padding: 3px 8px; border: 1.5px solid var(--hairline); border-radius: 3px; color: var(--fog); background: var(--void); cursor: pointer; }
  .chip .num { margin-left: 6px; opacity: 0.75; }
  .chip.on { border-color: var(--fog); background: var(--fog); color: var(--void); }
  .chip:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .plate { font-family: var(--font-mono); font-weight: 600; font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; padding: 3px 8px; border-radius: 3px; white-space: nowrap; }
  .plate.live { background: var(--clear); color: var(--on-clear); }
  .plate.troubled { background: var(--troubled); color: var(--on-troubled); }
  .plate.stale, .plate.reading { color: var(--static); outline: 1.5px dashed var(--static); outline-offset: -1.5px; }

  .scroller { overflow-x: auto; border: 1px solid var(--hairline); border-radius: 4px; padding: 10px 8px 8px; }
  .stage { display: flex; align-items: flex-start; gap: 14px; min-width: min-content; }
  .col { position: relative; flex: none; padding-top: 48px; }
  .col::before { content: ''; position: absolute; left: 0; right: calc(-1 * var(--gap)); top: 14px; height: 6px; background: var(--line); }
  .col.last::before { right: 0; border-radius: 0 3px 3px 0; }
  .stage.hollow .col::before { background: var(--void); box-shadow: inset 0 0 0 2px var(--static); }
  .dot { position: absolute; top: 8px; left: 50%; width: 18px; height: 18px; margin-left: -9px; border-radius: 50%; background: var(--void); border: 4px solid var(--fog); box-sizing: border-box; }
  .dot.trb { border-color: var(--troubled); }
  .dot.none { border-style: dashed; border-color: var(--static); }
  .stoplbl { position: absolute; top: 30px; left: 50%; transform: translateX(-50%); font-family: var(--font-mono); font-size: 10.5px; color: var(--static); white-space: nowrap; background: var(--void); padding: 0 4px; }

  .st { position: relative; border: 2px solid var(--fog); border-radius: 4px; background: var(--void); }
  .st.trb { border-color: var(--troubled); }
  .st.nostation { border: 1.5px dashed var(--static); min-height: 120px; }
  .inner { position: relative; }
  .st header { position: absolute; left: 0; right: 0; top: 0; display: flex; align-items: center; gap: 6px; height: 24px; padding: 0 6px 0 8px; border-bottom: 1px solid var(--hairline); white-space: nowrap; overflow: hidden; }
  .st.nostation header { position: static; }
  .st header b { font-size: 12.5px; }
  .st header .k { font-size: 10.5px; color: var(--static); overflow: hidden; text-overflow: ellipsis; }
  .st header .plate { margin-left: auto; padding: 2px 6px; font-size: 9.5px; }
  .nonote { margin: 6px 8px; font-size: 11px; color: var(--static); }
  svg { position: absolute; left: 0; top: 0; overflow: visible; pointer-events: none; }
  .p { fill: none; stroke: var(--fog); stroke-width: 1.5; }
  .p.idle { stroke: var(--static); stroke-dasharray: 3 3; }
  .bundle { fill: none; stroke: var(--fog); stroke-width: 11; stroke-linecap: round; }
  .bl { font-family: var(--font-mono); font-size: 10.5px; fill: var(--void); font-weight: 600; }
  .node { fill: var(--void); stroke: var(--fog); stroke-width: 1.5; }
  .node.idle { stroke: var(--static); stroke-dasharray: 3 2; }

  .adm { position: absolute; left: 6px; top: 32px; width: 50px; display: flex; flex-direction: column; gap: 3px; align-items: flex-start; }
  .adm .h { font-size: 10px; color: var(--static); }
  .tg { box-sizing: border-box; width: 48px; height: 13px; border: 1.5px dashed var(--fog); border-radius: 2px; font-size: 8.5px; line-height: 10px; padding: 0 2px; color: var(--fog); text-decoration: none; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .adm .n { font-size: 15px; font-weight: 600; line-height: 1.1; margin-top: 2px; }
  .adm .n.hot { color: var(--troubled-ink); }
  .adm .a { font-size: 10px; color: var(--static); }

  .bq { position: absolute; width: 56px; display: flex; flex-direction: column; align-items: center; gap: 2px; }
  .bq .lab { position: static; }
  .bq .n { font-size: 10px; margin-top: 14px; }
  .pt { position: absolute; box-sizing: border-box; height: 16px; display: flex; align-items: center; gap: 4px; font-size: 10.5px; white-space: nowrap; overflow: hidden; background: var(--void); padding-right: 2px; color: var(--fog); text-decoration: none; }
  a.pt:hover .t { text-decoration: underline; }
  .c { box-sizing: border-box; width: 11px; height: 11px; border: 1.5px solid var(--fog); border-radius: 2px; flex: none; background: var(--void); display: inline-block; }
  .c.on { background: var(--fog); }
  .pt.free .c { border-style: dashed; border-color: var(--static); }
  .pt.free .t { color: var(--static); }
  .mk { font-family: var(--font-mono); font-size: 10px; }
  .pt .t { overflow: hidden; text-overflow: ellipsis; min-width: 0; flex: 1; }
  .pt .d { font-size: 9.5px; color: var(--static); }
  .lab { position: absolute; font-size: 9.5px; color: var(--static); font-family: var(--font-mono); white-space: nowrap; }
  .arrived { outline: 2px solid var(--signal); outline-offset: 1px; }
  .foot { list-style: none; margin: 0; padding: 4px 8px 6px; border-top: 1px solid var(--hairline); font-size: 10.5px; color: var(--static); line-height: 1.3; }

  .legend { display: flex; gap: 14px; flex-wrap: wrap; font-size: 11px; color: var(--static); margin: 6px 0 0; }
  .legend span { display: inline-flex; align-items: center; gap: 5px; }
  .legend i { display: inline-block; box-sizing: border-box; width: 18px; height: 10px; border: 1.5px solid var(--fog); border-radius: 2px; }
  .legend i.dash { border-style: dashed; }
  .legend i.on { background: var(--fog); width: 10px; }
  .legend i.bun { height: 7px; border: 0; background: var(--fog); border-radius: 4px; width: 22px; }

  .tray-band { margin-top: var(--s3); }
  .tray-band h3, .moves h3 { margin: 0 0 var(--s1); font-size: 14px; font-weight: 800; }
  .tray { border: 1.5px dashed var(--static); border-radius: 4px; padding: 8px 10px; display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; align-items: baseline; font-size: 12px; }
  .tray b.num { font-size: 16px; }
  .tray a { color: var(--signal); }
  .moves { margin-top: var(--s3); }
  .move { display: flex; flex-wrap: wrap; gap: 4px var(--s2); align-items: baseline; padding: 6px 0; border-top: 1px solid var(--hairline); font-size: 12.5px; }
  .move .at { font-size: 11.5px; }
  .move .t { color: var(--fog); font-weight: 600; }
  .move .route { font-family: var(--font-mono); font-size: 11.5px; }
</style>
