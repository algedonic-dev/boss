<svelte:options namespace="svg" />

<script lang="ts">
  // THE ROUTE LAYER — every route the server serves, drawn (design
  // e765b3fc, car R3 on feedback 84cba7e2). A `<g>` inside the transit
  // map's SVG, apart from the stations and the alarms, so the map's own
  // component stays the stations' and this one the track's — and so the
  // moves of car M2 can draw over exactly these paths without editing
  // either (route-layout.ts `routePath` answers the same path by the
  // same (from, to) key).
  //
  // WHAT IT DRAWS, all of it from the routes read (route-layout.ts
  // `sectionsOf`):
  //   * a SECTION per route between two stations, in its line's colour —
  //     red where the server says it is not flowing, a hollow tube where
  //     it cannot tell, dashed red where only the moves record supports it
  //     — with its waiting blocks and its trains, and a door to its
  //     panel;
  //   * an EXIT per station a terminal closes packets at (David,
  //     added_2026_09_25_david_offramps: "every packet that leaves the map
  //     must leave by a drawn route") — since backlog 3c8b5653 (design
  //     f0313eda, E2 chosen 2026-09-26) a FAN in the exits band under the
  //     line: one stub dropping from the station, a 45° branch per
  //     terminal in name order, each ending in a buffer stop on the one
  //     rail with its count and terminal written at 45° under it, and the
  //     fan headed in the band by its station and total. A declared
  //     terminal nothing left by is thin and faded; one packets left by
  //     that no source declares for their kind is dashed red;
  //   * an ENTRY per station packets are admitted at.
  // Every element carries `data-section="<from>→<to>"` and `data-from` /
  // `data-to`. Colours are --map-* tokens only (map-palette.test.ts, the
  // a-colour-is-a-token lint).
  import { navigate } from '@boss/web-kit/nav';
  import type { Border } from './borders';
  import { sectionHref } from './regions';
  import { exitNames } from './routes';
  import type { Band, FanBranch, Section } from './route-layout';
  import { sectionGround, stationLabel, trainsOf, waitingBlocks } from './transit';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { presentationHref } from './map-navigation';

  type Props = Readonly<{
    /** The served routes, laid out (route-layout.ts `sectionsOf`). */
    sections: ReadonlyArray<Section>;
    /** Each section's border reading, by its key — its rate, its queue,
     *  whether it flows. A section with none reads "no reading". */
    borders: ReadonlyMap<string, Border>;
    reduced: boolean;
    /** The exits band the fans stand in (route-layout.ts `layFans`),
     *  null when no exit is served; `width` is the drawing's. */
    band?: Band | null;
    width?: number;
    height?: number;
    /** The selected section's key, marked with a casing; null for none. */
    selected: string | null;
    /** The real moves ride these sections (flight `it-map-live`, car M2
     *  of design e765b3fc): the rate-replay blocks are then off, so no
     *  crossing is drawn twice, once as a picture of it. */
    live?: boolean;
    overview?: boolean;
  }>;
  let { sections, borders, reduced, selected, live = false, overview = false, band = null, width = 0, height = 0 }: Props = $props();

  const track = $derived(sections.filter((s) => s.kind === 'section'));
  const ramps = $derived(sections.filter((s) => s.kind !== 'section'));

  /** A section's name and nothing more: its rate, its queue and its
   *  verdict are the panel's (car N2). */
  const sectionTitle = (from: string, to: string): string => `the ${stationLabel(from)} → ${stationLabel(to)} section`;

  /** An exit names its total and its terminals; an entry, the steps
   *  that put a packet on the map there. */
  const rampTitle = (s: Section): string =>
    s.kind === 'exit' && s.fan !== null
      ? `leaves the map from ${stationLabel(s.from ?? '')}: ${s.fan.total} by ${s.fan.branches.map((b) => b.name).join(', ')}`
      : `enters the map at ${stationLabel(s.to ?? '')}: ${exitNames(s.route).join(', ')}`;

  /** A branch in words: its count, and what declares it — or that
   *  nothing on this exit does. */
  const branchTitle = (s: Section, b: FanBranch): string =>
    `${b.name}: ${b.count} left ${stationLabel(s.from ?? '')} by it · ` +
    (b.declared ? `declared by ${b.via.join(', ')}` : `observed only (${b.via.join(', ')}) — no source on this exit declares it`);

  /** How a branch is drawn: dashed red when undeclared, thin and faded
   *  when declared and nothing left by it, its line's colour otherwise. */
  const branchClass = (s: Section, b: FanBranch): string =>
    !b.declared ? 'undeclared' : b.count === 0 ? `idle line-${s.line}` : `line-${s.line}`;

  /** The label's anchor at 45° under a buffer stop. */
  const labelAt = (b: FanBranch): string => `translate(${b.end.x + 3} ${b.end.y + 11}) rotate(45)`;

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

<g class="routes" class:overview data-routes>
  {#if band !== null}
    <!-- THE EXITS BAND (design f0313eda E2): one place to read how
         packets leave the map. Not a route, so no data-section. -->
    <g data-exits-band>
      <rect x="12" y={band.top} width={Math.max(0, width - 24)} height={Math.max(0, height - band.top - 16)} rx="6" class="band" />
      <text x="28" y={height - 26} class="band-h">{overview ? 'EXITS · select a station for its terminals' : 'EXITS · where packets leave the map · a branch per terminal, every buffer on one rail'}</text>
    </g>
  {/if}
  {#each ramps as s (s.key)}
    <g class="ramp" data-ramp={s.kind} data-section={s.key} data-from={s.from ?? undefined} data-to={s.to ?? undefined}
      data-declared={s.declared ? 'true' : 'false'} data-jog={s.fan !== null && s.fan.jog > 0 ? 'true' : undefined}>
      <title>{rampTitle(s)}</title>
      <path d={s.d} class="ramp-line line-{s.line}" class:stub={s.fan !== null} class:undeclared={!s.declared} />
      {#if s.fan !== null}
        {@const fan = s.fan}
        {#if !overview || selected === s.from}
          <text x={fan.head.x} y={fan.head.y} class="fan-h" data-fan-head={s.from}>{stationLabel(s.from ?? '')} · off {fan.total}</text>
        {/if}
        {#each fan.branches as b (b.name)}
          <g class="branch-g" data-terminal={b.name} data-count={b.count} data-declared={b.declared ? 'true' : 'false'}>
            <title>{branchTitle(s, b)}</title>
            <path d={b.d} class="branch {branchClass(s, b)}" />
            <path d="M{b.end.x - 5} {b.end.y} H{b.end.x + 5}" class="buffer {branchClass(s, b)}" data-buffer={b.name} />
            {#if !overview || selected === s.from}
            <text transform={labelAt(b)} class="blabel"><tspan class="n" class:zero={b.count === 0}>{b.count}</tspan> <tspan
                class="t" class:bad={!b.declared}>{b.name.replace(/-/g, ' ')}</tspan></text>
            {/if}
          </g>
        {/each}
      {/if}
    </g>
  {/each}

  <!-- Each section is a door to its own panel: a wide unpainted stroke
       takes the click, so an 8-unit line is not a needle to aim at. The
       selected one stands in an ink casing. -->
  {#each track as s (s.key)}
    {@const from = s.from ?? ''}
    {@const to = s.to ?? ''}
    {@const b = borders.get(s.key)}
    {@const ground = sectionGround(b)}
    {@const waiting = waitingBlocks(s, b)}
    {@const trains = live ? null : trainsOf(b, reduced)}
    {@const href = presentationHref(sectionHref(from, to), overview)}
    {@const isSelected = selected === s.key}
    <a class="section-link" class:context-muted={overview && selected !== null && selected !== s.key && selected !== from && selected !== to} href={safeLinkHref(href)} data-section-link={s.key} data-selected={isSelected ? 'true' : undefined}
      aria-current={isSelected ? 'true' : undefined} aria-label={sectionTitle(from, to)}
      onclick={(e) => open(e, href)}>
      {#if isSelected}
        <path d={s.d} class="casing" data-selected-mark={s.key} />
      {/if}
      <path d={s.d} class="section line-{s.line}" class:held={ground === 'held'} class:unknown={ground === 'unknown'}
        class:undeclared={!s.declared}
        data-section={s.key} data-from={from} data-to={to} data-line={s.line} data-ground={ground}
        data-declared={s.declared ? 'true' : 'false'}>
        <title>{sectionTitle(from, to)}{s.declared ? '' : ' — observed, declared by no protocol or hand-off'}</title>
      </path>
      {#if ground === 'unknown' && s.declared}
        <!-- The tube's pale centre (backlog b9c88c61): no data-section,
             because it is part of the section's drawing, not a route. -->
        <path d={s.d} class="tube-core" data-tube-core aria-hidden="true" />
      {/if}
      <path d={s.d} class="hit" aria-hidden="true" />
    </a>
    {#each waiting.blocks as p, i (i)}
      <rect x={p.x - 4} y={p.y - 13} width="8" height="7" rx="1.5" class="waiting" data-waiting={s.key} />
    {/each}
    {#if waiting.more !== null}
      <text x={waiting.more.at.x - 8} y={waiting.more.at.y - 7} text-anchor="end" class="more" data-more={s.key}>+{waiting.more.n}</text>
    {/if}
    {#if trains !== null}
      {#each trains.begins as begin, i (i)}
        <rect x="-7" y="-4" width="14" height="8" rx="2" class="train line-{s.line}" data-train={s.key}>
          <animateMotion dur="{trains.dur.toFixed(3)}s" begin="{begin.toFixed(3)}s" repeatCount="indefinite"
            path={s.d} rotate="auto" />
        </rect>
      {/each}
    {/if}
  {/each}
</g>

<style>
  /* Every colour a --map-* token from styles.css with no fallback: the
     lines the Design department's --map-line-*, the stall and the
     undeclared the map's own bad edge, the words its ink and muted. */
  .line-delivery { stroke: var(--map-line-delivery); }
  .line-publish { stroke: var(--map-line-publish); }
  .line-siding { stroke: var(--map-line-siding); }
  .line-tenant { stroke: var(--map-line-tenant); }

  .section { fill: none; stroke-width: 8; stroke-linecap: round; stroke-linejoin: round; }
  .section.held { stroke: var(--map-bad-edge); }
  /* NO READING YET (backlog b9c88c61, David 2026-09-26): a hollow tube —
     the section itself, in its line's colour, is the casing, and a pale
     core rides on top. It was a 2 6 dash on this round-capped stroke,
     which drew a string of beads; dashed now means undeclared only. */
  .tube-core { fill: none; stroke: var(--map-line-unconfirmed-core); stroke-width: 4; stroke-linecap: round;
    stroke-linejoin: round; pointer-events: none; }
  /* OBSERVED, UNDECLARED (design e765b3fc §2b): packets moved this way
     and no protocol or hand-off declares it — dashed in the map's own
     trouble red, over its line, so the drawing says it is a finding. */
  .section.undeclared, .ramp-line.undeclared, .branch.undeclared {
    stroke: var(--map-bad-edge); stroke-dasharray: 9 6; stroke-linecap: butt; opacity: 1; }
  /* An exit or an entry: thinner than a section, because nothing waits
     on it — it is where the map begins or ends for a packet. */
  .ramp-line { fill: none; stroke-width: 4; stroke-linecap: round; }
  .ramp-line.stub { stroke-width: 5; stroke-linecap: butt; stroke-linejoin: round; }
  /* THE FAN (design f0313eda E2): a branch per terminal, a buffer on the
     rail. A declared terminal nothing left by is thin and faded; an
     undeclared one dashed red, one level down from the route's rule. */
  .branch { fill: none; stroke-width: 3.5; stroke-linejoin: round; }
  .branch.idle { stroke-width: 1.75; opacity: 0.45; }
  .branch.undeclared { stroke-width: 3; stroke-dasharray: 6 4; }
  .buffer { fill: none; stroke-width: 3.5; stroke-linecap: square; }
  .buffer.idle { stroke-width: 2; opacity: 0.45; }
  .buffer.undeclared { stroke: var(--map-bad-edge); }
  .band { fill: var(--map-surface); stroke: var(--map-rule); }
  text.band-h { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; fill: var(--map-muted); }
  text.fan-h { font-size: 11px; font-weight: 700; fill: var(--map-ink); }
  .overview text.band-h, .overview text.fan-h, .overview text.blabel { font-size: 26px; }
  text.blabel { font-size: 10.5px; fill: var(--map-muted); dominant-baseline: middle; }
  .blabel .n { font-family: var(--font-mono); font-weight: 700; fill: var(--map-ink); font-variant-numeric: tabular-nums; }
  .blabel .n.zero { font-weight: 400; fill: var(--map-muted); }
  .blabel .t.bad { fill: var(--map-bad-ink); font-weight: 600; }
  /* The section's door: a wide stroke nobody sees takes the click. */
  .section-link { cursor: pointer; }
  .section-link.context-muted { opacity: 0.55; }
  .section-link:focus-visible { outline: none; }
  .hit { fill: none; stroke: transparent; stroke-width: 22; stroke-linecap: round; pointer-events: stroke; }
  /* THE SELECTION'S MARK (car N2), in the map's ink: a casing under the
     selected section, as a transit diagram draws an interchange. Not a
     state colour, so the mark never reads as a verdict. */
  .casing { fill: none; stroke: var(--map-ink); stroke-width: 16; stroke-linecap: round; stroke-linejoin: round; }
  .section-link:focus-visible .section, .section-link:hover .section { stroke-width: 11; }

  .waiting { fill: var(--map-ink); }
  .train { fill: var(--map-surface); stroke-width: 2; }
  text.more { font-family: var(--font-mono); font-size: 9.5px; fill: var(--map-muted); font-variant-numeric: tabular-nums; }
</style>
