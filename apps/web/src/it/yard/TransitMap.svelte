<script lang="ts">
  // Approved d4dada70 overview (2026-10-02) keeps the recorded transit
  // world mounted through orientation and board entry. Legacy links keep
  // their flights; this explicit entry never substitutes rate replay.
  // THE IT MAP AS A TRANSIT MONITOR (design 16091dfb, answered by David
  // 2026-09-25; backlog ced4ca8b) — behind the flight `it-map-transit`.
  // MapPage mounts this in place of the world map for a viewer the
  // flight lists; for everyone else the world map stays the map until
  // the flight is promoted (Q2).
  //
  // Each region is a STATION on a schematic line at fixed angles, each
  // route the server serves a SECTION of track in its line's colour, the
  // packets waiting to cross stand as blocks on the approach, a block
  // moves at the section's real crossing rate replayed ×60, a section the
  // server judges not flowing is drawn red on the track itself, and a
  // station's ring is its state — pulsing when troubled. The verdicts the
  // world map writes inside a territory stand on an ALARMS BOARD beside
  // the map instead: every non-clear station with the server's own why,
  // troubled first. A station, and an alarm, selects that station — the
  // same route a territory selects.
  //
  // ONLY SERVED ROUTES ARE DRAWN (design e765b3fc, car R3). The sections
  // are `GET /api/yard/routes`, laid out by route-layout.ts and drawn by
  // RouteLayer.svelte: the train's own line out of the dock, back to the
  // gates and over to the track, the garage's sidings both ways, every
  // packet's EXIT as a fan in the exits band under the line (backlog
  // 3c8b5653, design f0313eda E2: a stub per station, a branch per
  // terminal with its count, every buffer on one rail), every ENTRY as
  // a stub into its station — and a route only the moves record
  // supports, observed but declared by no protocol or hand-off, dashed
  // red. While the routes are
  // unread the stations still stand and no section is drawn: a guessed
  // track would be the hand-drawn map this car deleted.
  //
  // THE DETAIL LEAVES THE MAP (design e765b3fc, car N2; David
  // 2026-09-25: "put more of that data behind a map selection for
  // display at the bottom ... we don't need to keep it all on the main
  // map"). The map carries a station's name, its one number and its
  // state, and the piles; the headway written on every section, and the
  // why and flowing rule its hover titles carried, are the selection
  // panel's now (panel.ts). A section is a door like a station: a click
  // selects it (`/it?at=dock->track`), and whatever is selected is MARKED
  // on the map — a casing under a section, a ring round a station — drawn
  // apart from the station's own state ring, which is the state's alone.
  //
  // The layout and every word are transit.ts, unit-pinned; this owns the
  // strokes and the motion. Colours are --map-* tokens only — the lines
  // are the Design department's --map-line-* (styles.css), the rings the
  // map's own states — so map-palette.test.ts and the a-colour-is-a-token
  // lint hold this file to the one palette. Reduced motion is honoured:
  // no block moves and no ring pulses, and a section's panel carries the
  // rate.
  import { navigate } from '@boss/web-kit/nav';
  import { MediaQuery } from 'svelte/reactivity';
  import { flightOn } from '@boss/web-kit/session/flights.svelte';
  import LiveMotion from './LiveMotion.svelte';
  import { geometryOf } from './live-geometry';
  import { TRANSIT_CHOICES, liveText } from './live-motion';
  import { loadTransit, saveTransit } from './transit-choice';
  import type { Border, Borders } from './borders';
  import { countText, regionHref, type Region, type Regions } from './regions';
  import type { Routes } from './routes';
  import { NOT_IN_SCOPE } from '../../policy/withheld';
  import { ledgerOf, linesOf, mapView, sectionKey, sectionsOf, wordsSide } from './route-layout';
  import RouteLayer from './RouteLayer.svelte';
  import {
    LINE_LABEL,
    NAME_ABOVE,
    REPLAY_TEXT,
    STATIONS,
    alarmsOf,
    ringOf,
    stationCount,
    stationLabel,
  } from './transit';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { presentationHref } from './map-navigation';

  type Props = Readonly<{
    regions: Regions;
    /** The routes read (design e765b3fc, car R3), or null while unread or
     *  unreadable: the stations still stand, and no section is drawn —
     *  the page says the read failed. */
    routes?: Routes | null;
    /** The borders read, or null while unread or unreadable: a section
     *  then reads "no reading" and nothing waits or moves on it. */
    borders?: Borders | null;
    /** What the page has selected, in the map's own keys — a station's
     *  name or a section's `from→to` (selection.ts `markOf`); null for
     *  nothing. Marked, never re-read: the page decides what is selected. */
    selected?: string | null;
    overview?: boolean;
  }>;
  let { regions, routes = null, borders = null, selected = null, overview = false }: Props = $props();

  const prefersReduced = new MediaQuery('(prefers-reduced-motion: reduce)');
  const reduced = $derived(prefersReduced.current);
  /** REAL PACKETS MOVE (design e765b3fc car M2, flight `it-map-live`):
   *  the recorded moves ride the sections, and the rate-replay blocks
   *  are off — a replayed rate beside the real moves would draw each
   *  crossing twice, once as a picture of it. */
  const live = $derived(overview || flightOn('it-map-live'));
  let motionSource = $state({ kind: 'connecting', why: '' });
  function showFeed(kind: string, why: string): void { motionSource = { kind, why }; }

  /** How long a live dot takes to cross a section — the viewer's own,
   *  remembered in this browser (transit-choice.ts; David 2026-09-27,
   *  backlog 7c581d3d). Storage is reached only inside its guards, so a
   *  browser that refuses it runs on the default. */
  const store = (): Storage => localStorage;
  let transit = $state(loadTransit(store));
  function choose(ms: number): void {
    transit = ms;
    saveTransit(store, ms);
  }

  const byName = $derived(new Map(regions.regions.map((r) => [r.name, r] as const)));
  const byKey = $derived(new Map<string, Border>((borders?.borders ?? []).map((b) => [sectionKey(b.from, b.to), b])));
  const alarms = $derived(alarmsOf(regions));

  /** The served routes, laid out — and any this layout has no station
   *  for, said at the foot rather than dropped. */
  const laid = $derived(routes === null ? { sections: [], unplaced: [], band: null } : sectionsOf(routes));
  /** The drawing's size: the stations, and the exits band under them
   *  when any exit is served (design f0313eda E2). */
  const view = $derived(mapView(laid.sections.flatMap((s) => (s.fan === null ? [] : [s.fan])), laid.band));

  /** A station's ledger line — moves in · on · off, off the served
   *  counts — or null while the moves record is unread. */
  const ledger = (name: string): string | null => {
    const l = routes === null ? null : ledgerOf(routes, name);
    return l === null ? null : `in ${l.in} · on ${l.on} · off ${l.off}`;
  };
  const lines = $derived(linesOf(laid.sections));
  const anyExit = $derived(laid.sections.some((s) => s.kind === 'exit'));
  const anyUndeclared = $derived(laid.sections.some((s) => !s.declared));

  /** A station's name, its one number and its state — what the map
   *  carries. The why is the panel's. */
  const stationTitle = (name: string, r: Region | undefined): string =>
    r === undefined
      ? `${stationLabel(name)} · troubled · no reading`
      : `${stationLabel(name)} · ${countText(r)} · ${r.state}`;

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

<section class="transit" data-transit data-overview={overview ? 'true' : undefined} data-motion={reduced ? 'reduced' : 'moving'}>
  <div class="grid" class:overview>
    <div class="board">
      <svg
        viewBox="0 0 {view.width} {view.height}"
        role="group"
        aria-label="the IT network as a transit map: stations on their lines, the traffic on every section">
        <!-- THE ROUTES (design e765b3fc, car R3): every section, exit and
             entry the routes read serves, drawn by RouteLayer.svelte from
             route-layout.ts — the layer the moves of car M2 travel. -->
        <RouteLayer sections={laid.sections} borders={byKey} {reduced} {selected} {live} {overview} band={laid.band}
          width={view.width} height={view.height} />

        <!-- THE STATIONS: a ring each in the region's state, pulsing when
             troubled, and a door to its panel. The selected one wears a
             second, ink ring outside its own — the mark is the
             selection's, the inner ring stays the state's. -->
        {#each STATIONS as st (st.name)}
          {@const r = byName.get(st.name)}
          {@const state = ringOf(r)}
          {@const isSelected = selected === st.name}
          {@const side = wordsSide(st.name, laid.sections)}
          {@const wx = side === 'left' ? st.x - (overview ? 38 : 14) : st.x + (overview ? 38 : 14)}
          {@const anchor = side === 'left' ? 'end' : 'start'}
          {@const led = ledger(st.name)}
          <a class="station" href={safeLinkHref(presentationHref(regionHref(st.name), overview))} data-station={st.name} data-state={state}
            data-selected={isSelected ? 'true' : undefined} aria-current={isSelected ? 'true' : undefined}
            aria-label={stationTitle(st.name, r)} onclick={(e) => open(e, presentationHref(regionHref(st.name), overview))}>
            <title>{stationTitle(st.name, r)}</title>
            {#if isSelected}
              <circle cx={st.x} cy={st.y} r={overview ? 28 : 20} class="sel-ring" data-selected-mark={st.name} />
            {/if}
            {#if state === 'troubled' && !reduced}
              <circle cx={st.x} cy={st.y} r="15" class="pulse" data-pulse={st.name} />
            {/if}
            <!-- The door's face: the ring and its name. A station's
                 words now stand beside its ring rather than under it, so
                 an unpainted plate behind the ring and the name keeps the
                 door's centre on the door (a click at the middle of it
                 lands on it, not on the map behind). -->
            <rect x={st.below ? (side === 'left' ? st.x - 100 : st.x - 16) : st.x - (overview ? 75 : 45)}
              y={st.below ? st.y - 16 : st.y - NAME_ABOVE - 14} width={st.below ? 116 : overview ? 150 : 90}
              height={st.below ? 50 : NAME_ABOVE + 30} class="door" aria-hidden="true" />
            <circle cx={st.x} cy={st.y} r={overview ? 18 : 12} class="ring {state}" />
            <!-- WORDS OFF THE LINES (design f0313eda E2): the exit's stub
                 drops straight down from the ring, so a station off the
                 line writes its name beside it, and every station its
                 count and its in · on · off, on the side no section
                 leaves downward from. -->
            {#if st.below}
              <text x={wx} y={st.y + 28} text-anchor={anchor} class="stn">{stationLabel(st.name)}</text>
            {:else}
              <text x={st.x} y={st.y - NAME_ABOVE} text-anchor="middle" class="stn">{stationLabel(st.name)}</text>
            {/if}
          </a>
          {#if !overview}
            <text x={wx} y={st.below ? st.y + 41 : st.y + 34} text-anchor={anchor} class="sub words">{stationCount(r)}</text>
          {/if}
          {#if led !== null && !overview}
            <text x={wx} y={st.below ? st.y + 53 : st.y + 47} text-anchor={anchor} class="led words" data-ledger={st.name}>{led}</text>
          {/if}
        {/each}

        {#if live}
          <!-- THE MOVES (car M2): over the stations, so a ping reads on top. -->
          <LiveMotion geometry={geometryOf(routes)} {reduced} {transit} onFeed={overview ? showFeed : undefined} />
        {/if}
      </svg>
    </div>

    <!-- THE ALARMS BOARD: every station that is not clear, with the
         server's own why, troubled first. The why is printed whole —
         clamped on screen, never cut in the record. -->
    <aside class="alarms" aria-label="alarms">
      <h2>Alarms</h2>
      {#each alarms as a (a.name)}
        <a class="alarm" href={safeLinkHref(presentationHref(regionHref(a.name), overview))} data-alarm={a.name} data-state={a.state} title={a.why}
          onclick={(e) => open(e, presentationHref(regionHref(a.name), overview))}>
          <span class="alarm-head"><span class="alarm-name">{a.label}</span><span class="st {a.state}">{a.state}</span></span>
          {#if !overview}<span class="why">{a.why}</span>{/if}
        </a>
      {:else}
        <p class="none" data-alarms-clear>No alarms — every station is clear or full.</p>
      {/each}
    </aside>
  </div>
  {#if overview}
    <p class="motion-source" class:load-failed={motionSource.kind === 'down'} data-motion-status>
      Recorded moves: {motionSource.kind}{motionSource.why ? ` — ${motionSource.why}` : ''}. Display transit is {transit / 1000}s; the record's event time is unchanged.
    </p>
  {/if}

  <details open={!overview} class="map-key">
  <summary>Routes, counts and motion · {routes === null ? 'route reading unavailable' : `${routes.window_hours}h observation window`}</summary>
  <div class="key" aria-label="the lines">
    {#each lines as l (l)}
      <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="line-{l}" /></svg>{LINE_LABEL[l]}</span>
    {/each}
    <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="held" /></svg>a held section: the server judges nothing is crossing</span>
    <span class="key-item" data-key-unconfirmed><svg class="swatch tube" viewBox="0 0 20 6" aria-hidden="true"><line x1="0" y1="3" x2="20" y2="3" class="line-siding" /><line x1="0" y1="3" x2="20" y2="3" class="tube-core" /></svg>hollow: no reading yet, so the server cannot say whether it flows</span>
    {#if anyExit}
      <span class="key-item" data-key-exit><svg class="swatch tall" viewBox="0 0 20 12" aria-hidden="true"><path d="M10 0 V4 L4 10 M10 4 V10 M10 4 L16 10" class="line-delivery" /><line x1="1" y1="11" x2="19" y2="11" class="line-delivery" /></svg>an exit: one stub, a branch per terminal, a buffer stop and its count</span>
      <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="line-delivery idle" /></svg>faded: a declared terminal nothing left by in the window</span>
      <span class="key-item" data-key-ledger>in · on · off: moves into a station, on to another, off the map</span>
    {/if}
    {#if routes?.observed_withheld}
      <!-- The moves counts withheld from this caller (bd506215): said in
           the key, neutral, so a map with no counts never reads as a
           yard nothing crossed. -->
      <span class="key-item" data-key-withheld title={routes.observed_withheld}>moves counted: {NOT_IN_SCOPE}</span>
    {/if}
    {#if anyUndeclared}
      <span class="key-item" data-key-undeclared><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="undeclared" /></svg>dashed red: packets moved this way, and no protocol or hand-off declares it</span>
    {/if}
  </div>
  {#if laid.unplaced.length > 0}
    <p class="unplaced" data-unplaced>served, with no station on this map to draw it at: {laid.unplaced.join(', ')}</p>
  {/if}
  {#if live && !reduced}
    <!-- THE CROSSING TIME (David 2026-09-27: "maybe give me a few radio
         buttons"): the next dot to set off takes this long; one already
         in flight keeps its own. Nothing travels under reduced motion,
         so there is nothing to time. -->
    <fieldset class="speed" data-transit-choice={transit}>
      <legend>a crossing is shown over</legend>
      {#each TRANSIT_CHOICES as ms (ms)}
        <label class="speed-item">
          <input type="radio" name="live-transit" value={ms} checked={transit === ms} data-transit={ms}
            onchange={() => choose(ms)} />{ms / 1000} s
        </label>
      {/each}
    </fieldset>
  {/if}
  <p class="replay" data-replay>
    {live ? liveText(transit) : REPLAY_TEXT}{reduced && live ? '. Reduced motion is on: a move flashes its count at the station it reached, and nothing travels.' : reduced ? '. Reduced motion is on: nothing moves, and a section\'s panel carries its rate.' : ''}
  </p>
  </details>
</section>

<style>
  /* Every colour a --map-* token from styles.css with no fallback: the
     lines the Design department's --map-line-*, the rings the map's own
     ok / warn / bad edges, the words its ink and muted. */
  .transit { margin-top: var(--s3); }
  .grid { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: var(--s3); align-items: start; }
  .grid.overview { grid-template-columns: minmax(0, 1fr) 200px; }
  .map-key { margin-top: var(--s2); font-size: 12px; color: var(--map-muted); }
  .map-key summary { cursor: pointer; }
  @media (max-width: 1100px) { .grid, .grid.overview { grid-template-columns: minmax(0, 1fr); } }
  .board { background: var(--map-bg); border: 1px solid var(--map-rule); border-radius: var(--radius); overflow-x: auto; }
  /* Twice as wide since the exits band (design f0313eda E2), so it keeps
     a legible floor and scrolls across below it rather than shrinking. */
  .board svg { display: block; width: 100%; min-width: 1100px; height: auto; font-family: var(--font-body); }
  .overview .board svg { min-width: 0; }
  .overview .board text.stn { font-size: 30px; }
  .overview .board :global(.live text) { font-size: 30px; }
  .overview .board :global(.live .mark) { r: 10px; }
  .motion-source { margin: var(--s2) 0; color: var(--map-muted); font-size: 12px; overflow-wrap: anywhere; }
  .motion-source.load-failed { color: var(--map-bad-ink); }
  .board text { fill: var(--map-ink); }
  .stn { font-size: 12px; font-weight: 600; }
  .sub { font-size: 10.5px; fill: var(--map-muted); }
  .board text.sub { fill: var(--map-muted); }
  .board text.led { font-size: 9.5px; fill: var(--map-muted); font-variant-numeric: tabular-nums; }

  .line-delivery { stroke: var(--map-line-delivery); }
  .line-publish { stroke: var(--map-line-publish); }
  .line-siding { stroke: var(--map-line-siding); }
  .line-tenant { stroke: var(--map-line-tenant); }
  .swatch line.held { stroke: var(--map-bad-edge); }

  /* THE SELECTION'S MARK (car N2), in the map's ink: a ring outside the
     selected station's own (a section's casing is RouteLayer's). Not a
     state colour, so the mark never reads as a verdict. */
  .sel-ring { fill: none; stroke: var(--map-ink); stroke-width: 3; }
  /* The key's swatch for an observed, undeclared route — drawn as
     RouteLayer draws the route itself. */
  .swatch line.undeclared { stroke: var(--map-bad-edge); stroke-dasharray: 9 6; stroke-linecap: butt; }

  .station { cursor: pointer; }
  .door { fill: transparent; pointer-events: all; }
  .words { pointer-events: none; }
  .station:focus-visible { outline: none; }
  .station:focus-visible .ring { stroke-width: 6; }
  .ring { fill: var(--map-surface); stroke-width: 5; }
  .ring.clear { stroke: var(--map-ok-edge); stroke-width: 3; }
  /* FULL (design e765b3fc Q2, David 2026-09-25): at capacity and moving
     is a SOLID disk with a heavier ring — clear stays hollow — so "the
     gates are full" reads by fill alone, without the small text. */
  .ring.full { fill: var(--map-full); stroke: var(--map-full); stroke-width: 6; }
  .ring.attention { stroke: var(--map-warn-edge); }
  .ring.troubled { stroke: var(--map-bad-edge); }
  .pulse { fill: none; stroke: var(--map-bad-edge); stroke-width: 3; transform-box: fill-box; transform-origin: center;
    animation: pulse 1.6s ease-out infinite; }
  @keyframes pulse { from { transform: scale(1); opacity: 0.9; } to { transform: scale(1.75); opacity: 0; } }
  /* Belt and braces: the pulse is not rendered under reduced motion,
     and a stylesheet that outlives that decision still does not move. */
  @media (prefers-reduced-motion: reduce) { .pulse { animation: none; } }

  .alarms { background: var(--map-surface); border: 1px solid var(--map-rule); border-top: 3px solid var(--map-ink);
    border-radius: var(--radius); padding: var(--s2) var(--s3); }
  .alarms h2 { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-nav); text-transform: uppercase;
    margin: 0 0 var(--s2); color: var(--map-ink); }
  .alarm { display: block; border-top: 1px solid var(--map-rule); padding: var(--s2) 0; color: var(--map-ink); text-decoration: none; }
  .alarm:first-of-type { border-top: 0; }
  .alarm:hover .alarm-name, .alarm:focus-visible .alarm-name { text-decoration: underline; }
  .alarm-head { display: flex; justify-content: space-between; gap: var(--s2); font-size: 13px; font-weight: 600; }
  .st { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-label); text-transform: uppercase; }
  .st.troubled { color: var(--map-bad-ink); }
  .st.attention { color: var(--map-warn-ink); }
  .why { display: -webkit-box; -webkit-line-clamp: 4; line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden;
    margin-top: 3px; font-size: 12px; color: var(--map-muted); overflow-wrap: anywhere; }
  .none { margin: 0; font-size: 12px; color: var(--map-muted); }

  .key { display: flex; flex-wrap: wrap; gap: 6px 16px; margin-top: var(--s2); font-size: 12px; color: var(--map-muted); }
  .key-item { display: inline-flex; align-items: center; gap: 6px; }
  .swatch { width: 20px; height: 4px; }
  .swatch line { stroke-width: 4; }
  .swatch.tall { height: 12px; }
  .swatch.tall path, .swatch.tall line { fill: none; stroke-width: 2; }
  .swatch line.idle { stroke-width: 1.5; opacity: 0.45; }
  /* The key's swatch for a section with no reading — drawn as RouteLayer
     draws the section itself: a casing with a pale core (b9c88c61). */
  .swatch.tube { height: 6px; }
  .swatch.tube line { stroke-width: 6; }
  .swatch.tube line.tube-core { stroke: var(--map-line-unconfirmed-core); stroke-width: 2.5; }
  .unplaced { margin: var(--s1) 0 0; font-size: 12px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
  .replay { margin: var(--s1) 0 0; font-size: 12px; color: var(--map-muted); }
  /* The crossing time: a line of the key's size and voice, not a panel. */
  .speed { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; margin: var(--s2) 0 0; padding: 0;
    border: 0; font-size: 12px; color: var(--map-muted); }
  .speed legend { float: left; padding: 0; margin-right: 4px; }
  .speed-item { display: inline-flex; align-items: center; gap: 4px; color: var(--map-ink); cursor: pointer;
    font-variant-numeric: tabular-nums; }
  .speed-item input { margin: 0; accent-color: var(--map-ink); }
</style>
