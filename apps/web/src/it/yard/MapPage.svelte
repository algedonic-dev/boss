<script lang="ts">
  // THE DEPARTMENT MAP — the /it landing (design e765b3fc; David,
  // 2026-09-25: "the operating map sits at the top always and clicking
  // stations or lines pulls up detail below"). The map on top — the
  // transit map for every viewer, the strip on
  // a phone — and, below it, the panel of whatever `/it?at=<name>`
  // selects (regions.ts `regionHref`): a station or a section of track.
  // A selection is a query on this one route, so the map is mounted once
  // and a click never tears it down.
  //
  // The map and panel use ordinary document scrolling (7bc8002d).
  // A selection brings the panel into view without retaining the map.
  //
  // WHAT A PAGE WAS IS A PANEL NOW (car N3). A region's floor was its own
  // page at /it/yard/<region>, with a second map of its own (design
  // d2154293 car 3, fe77a1d2, 62de32ae decision 7); car N2 drew the
  // floor's cards in the station's panel, and car N3 retired the page,
  // its map and its route. The Crew Board is the shop floor's panel, and
  // four more pages MOVED here before they retired (panel.ts
  // `STATION_BOARDS` is the table): yard status is the track's, dock's
  // and garage's, the conductor's feed the track's, and the feedback and
  // backlog boards receiving's (the backlog marshalling's too). No alias
  // and no redirect: each old path is not found, and its one door is
  // this map.
  //
  // NO NEW STYLING (the visual redesign reskins): the map is drawn in the
  // yard's own strokes and tokens.
  //
  // Every number is read from ONE endpoint, /api/yard/regions (the rails
  // from /api/yard/borders), on the yard's 10s tick, through fetchRemote
  // — so an outage renders a failure line (`load-failed`), never an empty
  // map that reads as a calm one.
  import { onMount, untrack } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { countText, fetchRegions, lampOf, regionHref, sectionHref, stateText, type Regions } from './regions';
  import { markOf, selectionOf, unreadStationOf, type MapSelection } from './selection';
  import { boardsOf, sectionCells, stationCells, type PanelCell } from './panel';
  import { hasInterior } from './region-contents';
  import { hasPlatforms } from './world-interior';
  import { fetchBorders, type Borders } from './borders';
  import { fetchRoutes, type Routes } from './routes';
  import { fetchFlow, type Flow } from './flow';
  import { NEEDS_YOU_URL, parseNeedsYou, outranksBoard, needsYouBoard, type Figure, type LastGood, type NeedsYou } from './hud';
  import { presentationHref } from './map-navigation';
  import HudFrame from './HudFrame.svelte';
  import TransitMap from './TransitMap.svelte';
  import PlantStrip from './PlantStrip.svelte';
  import FloorDeck from './FloorDeck.svelte';
  import StationBoundary from './StationBoundary.svelte';
  import YardStatusPanel from './YardStatusPanel.svelte';
  import ConductorFeed from '../monitoring/ConductorFeed.svelte';
  import CrewBoardPage from '../crew/CrewBoardPage.svelte';
  import SensorsPanel from '../sensors/SensorsPanel.svelte';
  import ReceivingYardPage from '../receiving/ReceivingYardPage.svelte';
  import MarshallingYardPage from '../marshalling/MarshallingYardPage.svelte';
  import FeedbackTriagePage from '../feedback/FeedbackTriagePage.svelte';
  import BacklogBoardPage from '../backlog/BacklogBoardPage.svelte';
  import { MediaQuery } from 'svelte/reactivity';
  import PhoneStrip from './PhoneStrip.svelte';
  import { PHONE_QUERY } from './phone-strip';
  import { mapRead, startMapPolling } from './map-polling';

  type Props = Readonly<{
    /** The Department Map's selection — `/it?at=<name>`. Absent, nothing
     *  is selected and no panel opens. */
    at?: string;
    overview?: boolean;
    detail?: boolean;
  }>;
  let { at = undefined, overview = false, detail = false }: Props = $props();

  /** A phone: the world is drawn as the strip map rather than the SVG
   *  shrunk (design 62de32ae decision 12, car G). ONE of the two is
   *  mounted, never both hidden by CSS — a hidden world would still be
   *  read, counted and crawled as if it were on the screen. */
  const phone = new MediaQuery(PHONE_QUERY);
  // Both map flights were promoted by David, 2026-10-23 (54c04706,
  // 71c4d42e): the transit monitor and recorded moves are the default.

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let borders = $state<Remote<Borders>>({ kind: 'loading' });
  let flow = $state<Remote<Flow>>({ kind: 'loading' });
  /** The routes the map draws (design e765b3fc, car R3): every section,
   *  exit and entry is one the server serves, derived from the
   *  protocols — the page holds no edge of its own. */
  let routes = $state<Remote<Routes>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);
  /** The newest regions read that succeeded, and when — the HUD's
   *  "last good HH:MMZ" when a later read fails (design 00774ca8
   *  decision 5). Its VALUES are never drawn once a newer read failed. */
  let lastGood = $state<LastGood | null>(null);
  let successfulReads = $state(0);
  /** The top board's NEEDS YOU row (design ea906603 Q2): the viewer's
   *  own queue, their aliases and roles resolved by the server. A read of
   *  its own, because the regions read is the same for every viewer. */
  let needs = $state<Remote<NeedsYou>>({ kind: 'loading' });

  onMount(() => {
    // One read in flight per endpoint (452e507c, 909adf3b): a tick skips
    // a read still out, so slow reads no longer stack into 524s; a slow
    // read holds only itself and never renews another endpoint's time;
    // a hidden tab adds no background reads. map-polling.ts says why.
    return startMapPolling([
      mapRead(fetchRegions, (r, at) => {
        regions = r;
        readAt = at;
        if (r.kind === 'ready') {
          lastGood = { at };
          successfulReads += 1;
        }
      }),
      mapRead(fetchBorders, (b) => {
        borders = b;
      }),
      mapRead(fetchRoutes, (w) => { routes = w; }),
      mapRead(fetchFlow, (f) => { flow = f; }),
      mapRead(() => fetchRemote(NEEDS_YOU_URL, parseNeedsYou), (n) => { needs = n; }),
    ], {
      hidden: () => document.hidden,
      every: (tick, ms) => {
        const timer = setInterval(tick, ms);
        return () => clearInterval(timer);
      },
      onVisibility: (tick) => {
        document.addEventListener('visibilitychange', tick);
        return () => document.removeEventListener('visibilitychange', tick);
      },
    });
  });

  const clock = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  /** What `?at=` selects, against the regions the server served — none
   *  while the read is out. */
  const picked = $derived<MapSelection>(
    regions.kind === 'ready'
      ? selectionOf(
          at,
          regions.data,
          borders.kind === 'ready' ? borders.data : null,
          routes.kind === 'ready' ? routes.data : null,
        )
      : { kind: 'none' },
  );
  /** The panel's cells (design e765b3fc, car N2): every reading the map
   *  no longer writes, for the one station or section selected. */
  const cells = $derived<ReadonlyArray<PanelCell>>(
    picked.kind === 'station'
      ? stationCells(picked.region, borders.kind === 'ready' ? borders.data : null, regions.kind === 'ready' ? regions.data.now : '')
      : picked.kind === 'section'
        ? sectionCells(
            picked.border,
            borders.kind === 'ready' ? borders.data.now : '',
            picked.route === null
              ? null
              : {
                  route: picked.route,
                  windowHours: routes.kind === 'ready' ? routes.data.window_hours : 24,
                  bordersRead: borders.kind === 'ready',
                  observedWithheld: routes.kind === 'ready' ? routes.data.observed_withheld : null,
                },
          )
        : [],
  );

  // The map scrolls with the document (7bc8002d). Bring a selected
  // panel into view once; the ten-second reads must not move the page.
  let panel = $state<HTMLElement | null>(null);
  $effect(() => {
    const selection = { at, detail };
    const el = panel;
    if (selection.at === undefined || el === null) return;
    untrack(() => {
      if (overview) el.focus({ preventScroll: true });
      el.scrollIntoView({ block: 'nearest' });
    });
  });

  function go(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }

  /** Where the boards of the retired pages live now — said on the page,
   *  so a reader who knew the old tab finds it (David, 2026-09-25: a
   *  newcomer loses nothing). panel.ts `STATION_BOARDS` is the table. */
  const WHERE =
    'Feedback and the IT backlog are in the Receiving station\'s panel (the backlog in Marshalling\'s too), the Crew Board in the Shop floor\'s, and yard status and the conductor\'s activity in the Track\'s, with the Dock\'s and the Garage\'s lanes in theirs.';
  // A floor — a count cut at its limit (backlog 97b5cf45) — reads `N+`.
  const figureText = (figure: Figure): string =>
    figure.kind === 'zero' ? '0' : figure.kind === 'unread' ? 'unread' : figure.kind === 'floor' ? `${figure.text}+` : figure.text;
  const urgent = $derived(outranksBoard(regions));
  const personal = $derived(needsYouBoard(needs, readAt ?? 0));
  const inspectionHref = $derived(picked.kind === 'station' ? regionHref(picked.name) : picked.kind === 'section' ? sectionHref(picked.from, picked.to) : '/it');
</script>

<div class="theme-exec yard-root">
  <!-- Named what its sidebar row is named (design e765b3fc, car N1):
       it answered to "Train Yard", "The IT world" and "IT · Forge
       line" at once (page audit gap f9850601). -->
  <a class="company-back" href="/map" onclick={(e) => go(e, '/map')}>Company System Map</a>
  <PageHeader
    eyebrow="IT"
    title={overview ? 'IT · Transit map' : 'Department Map'}
    subtitle={overview ? 'Follow recorded work. Select a station or route to orient, then open its detailed board.' :
      'The network as a transit monitor: each region a station on its line, each border a section of track with what waits on it, and the alarms board beside it. Select a station or a section to open its detail below the map. ' + WHERE}
  />

  <!-- THE TOP BOARD (design ea906603) in the HUD frame (design
       00774ca8): what outranks regular order, what needs you, what is
       next up and the machines, above the map. It stands whatever the
       reads did: a failed read turns its rows to `?` rather than taking
       the frame away, and each row fails alone. -->
  {#if overview}
    <details class="map-context">
      <summary>Outranks regular order: {urgent.kind === 'unread' ? 'unread' : figureText(urgent.count)} · Needs you: {personal.kind === 'unread' ? 'unread' : figureText(personal.count)} · priorities and machines</summary>
      <HudFrame read={regions} {readAt} {lastGood} {needs} />
    </details>
  {:else}
    <HudFrame read={regions} {readAt} {lastGood} {needs} />
  {/if}

  <p class="flow-reading" class:load-failed={flow.kind === 'failed'} role="status"
    data-flow-state={flow.kind === 'ready' ? flow.data.state : flow.kind}>
    {#if flow.kind === 'loading'}
      Reading flow counts…
    {:else if flow.kind === 'failed'}
      Flow counts unread — {flow.error}
    {:else if flow.data.state === 'pending'}
      Flow counts pending
    {:else if flow.data.state === 'unread'}
      Flow counts unread — {flow.data.why}
    {:else if flow.data.state === 'withheld'}
      Flow counts withheld — {flow.data.why}
    {:else}
      Flow counts read at <time datetime={flow.data.as_of}>{flow.data.as_of}</time>
      {#if flow.data.why !== null} · last refresh failed — {flow.data.why}{/if}
    {/if}
  </p>

  {#if regions.kind === 'loading'}
    <div class="yard-empty">Reading the regions…</div>
  {:else if regions.kind === 'failed'}
    <!-- A failed read is said, in the one class the outage crawl reads:
         a map that could not be read must not look like a clear one. -->
    <div class="yard-empty load-failed">The regions cannot be read — {regions.error}</div>
    {@const unread = unreadStationOf(at)}
    {#if unread !== null}
      <!-- A STATION WHOSE READING FAILED STILL OPENS (car N3): its
           readings came with the regions read, so they are said to be
           unread — but what stands in it reads its own endpoints, as the
           floor page it replaced did, and a failed regions read must not
           take those boards down with it. -->
      <section
        class="map-panel"
        bind:this={panel}
        data-map-panel
        data-selection={unread.name}
        data-kind="station"
        data-state="unknown"
        aria-label="the {unread.name} selection">
        <header class="panel-head">
          <span class="panel-kind">Station</span>
          <h2 class="panel-title">{unread.title}</h2>
          <a class="panel-close" data-close href={safeLinkHref(presentationHref('/it', overview))} aria-label="close the {unread.name} selection"
            onclick={(e) => go(e, presentationHref('/it', overview))}>close</a>
        </header>
        <p class="panel-none">Its readings come with the regions read, which failed; what stands in it reads on its own, below.</p>
        {#if overview && !detail}
          <a href={safeLinkHref(`${presentationHref(regionHref(unread.name), true)}&detail=1`)} onclick={(e) => go(e, `${presentationHref(regionHref(unread.name), true)}&detail=1`)}>Open detailed board</a>
        {:else}
          {@render station(unread.name)}
        {/if}
      </section>
    {/if}
  {:else}
    <!-- The map scrolls off with the page, leaving room for the floor. -->
    <div class="map-pin" data-map-pin="scrolls">
      {#if phone.current}
        <PhoneStrip
          regions={regions.data}
          borders={borders.kind === 'ready' ? borders.data : null} {overview} />
      {:else}
        <!-- The same two reads, drawn as a transit map (design 16091dfb):
             stations, sections and the alarms board, with the selection
             marked on it (design e765b3fc, car N2). -->
        <TransitMap
          regions={regions.data}
          routes={routes.kind === 'ready' ? routes.data : null}
          borders={borders.kind === 'ready' ? borders.data : null}
          selected={markOf(picked)} {overview} />
      {/if}
    </div>
    {#if !phone.current && !overview}
      <!-- THE PLANT along the map's edge (design 62de32ae, decision 11):
           the host runners serve every region, so they stand under the
           map rather than in one of its stations. -->
      <PlantStrip machines={regions.data.plant} />
    {/if}
    {#if picked.kind !== 'none'}
      <!-- THE SELECTION'S PANEL, under the map (design e765b3fc). Car N1
           opened it as a shell; car N2 filled it with what the map no
           longer writes — a station's or a section's rate, what waits
           and on whom, what is stuck, its trend, the band and the why
           that decided its state, its machines and its crossings
           (panel.ts) — and, for a station, what stands in it: its
           floor's cards. Car N3 added the boards of the pages that
           retired into it. A name the read did not carry is said, never
           drawn as a quiet empty panel. -->
      {@const name = picked.kind === 'station' ? picked.name : picked.kind === 'section' ? picked.key : picked.at}
      {@const state = picked.kind === 'station' ? picked.region.state : picked.kind === 'section' ? (picked.border?.state ?? 'unknown') : 'unknown'}
      <section
        class="map-panel"
        bind:this={panel}
        data-map-panel
        data-map-overview={overview && !detail ? 'true' : undefined}
        tabindex="-1"
        data-selection={name}
        data-kind={picked.kind}
        data-state={state}
        aria-label="the {name} selection">
        <header class="panel-head">
          {#if state !== 'unknown'}
            <span class="panel-lamp {lampOf(state)}" aria-hidden="true"></span>
          {/if}
          <span class="panel-kind">{picked.kind === 'station' ? overview ? 'IT area' : 'Station' : picked.kind === 'section' ? overview ? 'Route' : 'Section' : 'Selection'}</span>
          <h2 class="panel-title">{picked.kind === 'unknown' ? name : picked.title}</h2>
          {#if overview && detail}
            <a href={safeLinkHref(presentationHref(inspectionHref, true))} onclick={(e) => go(e, presentationHref(inspectionHref, true))}>Back to overview</a>
          {/if}
          <a class="panel-close" data-close href={safeLinkHref(presentationHref('/it', overview))} aria-label="close the {name} selection"
            onclick={(e) => go(e, presentationHref('/it', overview))}>close</a>
        </header>
        {#if picked.kind === 'unknown'}
          <p class="panel-none">Nothing on this map is named “{name}”.</p>
        {:else}
          {#if picked.kind === 'station'}
            <div class="panel-reading">
              <span class="panel-figure" data-figure>{countText(picked.region)}</span>
              <span class="panel-state">{stateText(picked.region)}</span>
            </div>
          {/if}
          {#if overview && !detail}
            <div class="orientation">
              <p>{picked.kind === 'station' ? picked.region.why : picked.border?.why ?? 'The route reading is unavailable; its condition cannot be judged.'}</p>
              <p class="scope-note">{picked.kind === 'station' ? 'Department aggregate; detailed boards identify its queues or machinery.' : 'Protocol route; detailed inspection preserves its crossings and evidence.'} Observation: {picked.kind === 'station' ? regions.data.now || 'unknown' : borders.kind === 'ready' ? borders.data.now || 'unknown' : 'unread'}</p>
              <a class="board-door" href={safeLinkHref(`${presentationHref(inspectionHref, true)}&detail=1`)} onclick={(e) => go(e, `${presentationHref(inspectionHref, true)}&detail=1`)}>Open detailed board</a>
            </div>
          {:else}
          <div class="panel-cells">
            {#each cells as c (c.field)}
              <div class="panel-cell" data-field={c.field}>
                <h3 class="cell-label">{c.label}</h3>
                <ul class="cell-lines">
                  {#each c.lines as line, i (i)}
                    <li>{line}</li>
                  {/each}
                </ul>
              </div>
            {/each}
          </div>
          {#if picked.kind === 'station'}
            {@render station(picked.name)}
          {/if}
          {/if}
        {/if}
      </section>
    {/if}
    <!-- A failed rails read is SAID — the territories are still drawn,
         but a map whose rails could not be read must not look like a
         quiet one. -->
    {#if borders.kind === 'failed'}
      <div class="yard-empty load-failed">The borders cannot be read — {borders.error}</div>
    {/if}
    <!-- A failed routes read is said where the routes are drawn (car R3):
         the transit map's stations still stand, and no section is guessed
         in its place. -->
    {#if routes.kind === 'failed' && !phone.current}
      <div class="yard-empty load-failed" data-routes-failed>The routes cannot be read — {routes.error}</div>
    {/if}
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}
</div>

<!-- A STATION'S CONTENTS, under a net. A STATION THAT CANNOT BE DRAWN
     SAYS SO (backlog 846ab934): a board that throws on live data becomes
     the failure line the outage crawl reads, naming the error, and the
     map above it stands. -->
{#snippet station(name: string)}
  <div class="panel-contents" data-contents={name}>
    <StationBoundary {name} generation={successfulReads}>
      {@render contents(name)}
    </StationBoundary>
  </div>
{/snippet}

<!-- WHAT STANDS IN A STATION — its floor's cards, and the boards of the
     pages that retired into it (design e765b3fc, cars N2 and N3). Keyed
     on the station, so a move from one to another opens the new
     station's boards rather than keeping the old one's selection. -->
{#snippet contents(region: string)}
  {#key region}
    {#if hasInterior(region)}
      <!-- THE FLOOR'S DECK: the Train Yard's panels (design fe77a1d2,
           car 3) — the departure board and the entity panel, whose
           selection the board's own rows drive. -->
      <FloorDeck focus={region} />
    {:else if hasPlatforms(region)}
      <!-- THE QUEUE BOARD (car 4 of d2154293), with its page header
           dropped: the station's panel is its heading. -->
      {#if region === 'receiving'}
        <ReceivingYardPage embedded />
      {:else if region === 'shop-floor'}
        <CrewBoardPage embedded />
      {:else}
        <MarshallingYardPage embedded regionRead={regions} />
      {/if}
    {:else if region === 'sensors'}
      <SensorsPanel />
    {/if}
    {#each boardsOf(region) as board (board)}
      <div class="panel-board" data-station-board={board}>
        {#if board === 'yard-status'}
          <YardStatusPanel part={region === 'track' ? 'track' : region === 'dock' ? 'dock' : 'garage'} />
        {:else if board === 'conductor'}
          <ConductorFeed />
        {:else if board === 'feedback'}
          <FeedbackTriagePage />
        {:else}
          <BacklogBoardPage />
        {/if}
      </div>
    {/each}
  {/key}
{/snippet}

<style>
  .flow-reading { margin: 0 0 12px; font-size: 12px; color: var(--map-muted); }
  .company-back { display: inline-block; margin-bottom: var(--s2); color: var(--map-ink); font-size: 13px; }
  .map-context { margin-bottom: var(--s3); color: var(--map-muted); font-size: 13px; }
  .map-context summary { cursor: pointer; }
  .orientation { padding: var(--s3); background: var(--map-surface); border: 1px solid var(--map-rule); border-radius: var(--radius); }
  .scope-note { color: var(--map-muted); font-size: 12px; overflow-wrap: anywhere; }
  .board-door { display: inline-block; margin-top: var(--s2); padding: var(--s2) var(--s3); color: var(--map-ink); border: 1px solid var(--map-rule); border-radius: var(--radius); }
  /* The yard's classes, as FloorDeck.svelte declares them (Svelte scopes
     a component's styles, so the page carries its own copy of the ones
     it uses — same names, nothing new). The colours are the map's own
     --map-* tokens, with no fallback (42f66fb3, map-palette.test.ts). */
  .yard-root { padding: 0 32px 32px; }
  /* On a phone the shell's own 16px gutter is the page's (styles.css,
     car G); this page's 32px on top of it left the strip 294px of 390. */
  @media (max-width: 720px) { .yard-root { padding: 0 0 24px; } }
  .yard-empty { color: var(--map-muted); padding: 12px 0; font-size: 14px; }
  .yard-flow { font-family: var(--font-mono); font-size: 11px;
    letter-spacing: var(--ls-nav); color: var(--map-muted);
    border-top: 1px solid var(--map-rule); margin-top: 28px; padding-top: 12px; }
  /* The selection's panel — a departure-board plate under the map, in the
     crossing panel's grammar: a hairline frame, a mono
     uppercase head with the state's lamp, the figure large beneath. */
  .map-panel { margin-top: var(--s3); border: 1px solid var(--map-rule-strong);
    border-top-width: 3px; background: var(--map-surface); padding: var(--s2) var(--s3) var(--s3);
    color: var(--map-ink); }
  .map-panel[data-state='attention'] { border-top-color: var(--map-warn-edge); }
  .map-panel[data-state='troubled'], .map-panel[data-state='unknown'] { border-top-color: var(--map-bad-edge); }
  .panel-head { display: flex; align-items: center; gap: var(--s2); font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; }
  .panel-kind { color: var(--map-muted); }
  .panel-title { margin: 0; font: inherit; font-weight: 600; color: var(--map-ink); }
  .panel-lamp { width: 8px; height: 8px; border-radius: 50%; background: var(--map-rule-strong); }
  .panel-lamp.ok { background: var(--map-ok-edge); }
  .panel-lamp.warn { background: var(--map-warn-edge); }
  .panel-lamp.err { background: var(--map-bad-edge); }
  .panel-close { margin-left: auto; color: var(--map-muted); border: 1px solid var(--map-rule);
    padding: 2px 8px; text-decoration: none; }
  .panel-close:hover, .panel-close:focus-visible { color: var(--map-ink); border-color: var(--map-ink); }
  .panel-reading { display: flex; align-items: baseline; flex-wrap: wrap; gap: var(--s2) var(--s3);
    margin: var(--s2) 0; }
  .panel-figure { font-family: var(--font-mono); font-size: 22px; font-variant-numeric: tabular-nums; }
  .panel-state { font-family: var(--font-mono); font-size: 12px; color: var(--map-muted); }
  .map-panel[data-state='attention'] .panel-state { color: var(--map-warn-ink); }
  .map-panel[data-state='troubled'] .panel-state { color: var(--map-bad-ink); }
  /* The readings (car N2): a board of cells, each a mono label over the
     server's own sentences. The long lists — what waits, and the
     crossings — take the whole row, so a hold sentence is never
     squeezed into a column; `dense` lets the short cells close up the
     row a long one leaves behind. On a phone every cell is a row. */
  .panel-cells { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); grid-auto-flow: dense;
    gap: var(--s2); margin: var(--s2) 0 var(--s3); }
  .panel-cell { background: var(--map-bg); border: 1px solid var(--map-rule); padding: var(--s2); min-width: 0; }
  .panel-cell[data-field='route'], .panel-cell[data-field='waiting'], .panel-cell[data-field='crossings'] { grid-column: 1 / -1; }
  .cell-label { margin: 0 0 4px; font-family: var(--font-mono); font-size: 11px; font-weight: 400;
    letter-spacing: 0.08em; text-transform: uppercase; color: var(--map-muted); }
  .cell-lines { margin: 0; padding: 0; list-style: none; font-size: 13px; line-height: 1.45; color: var(--map-ink);
    overflow-wrap: anywhere; }
  .cell-lines li + li { margin-top: 2px; }
  .panel-contents { margin-top: var(--s3); border-top: 1px solid var(--map-rule); }
  .panel-board { margin-top: var(--s3); }
  .panel-none { margin: var(--s2) 0 0; font-size: 13px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
</style>
