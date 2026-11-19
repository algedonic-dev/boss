<script lang="ts">
  // THE FLOOR SPEAKS THE TREE — T7's drawing (design 4b27a65a, backlog
  // 94a9dad4; the bundle David approved 2026-11-06). The recursive station
  // card (StationCard.svelte) draws GET /api/stations?under= — the
  // company, its departments, their rooms, each with its allocation from
  // its parent and its constraints — and a line joins two cards where a
  // station row declares the edge.
  //
  // R3 of the transit-map retirement (backlog 23a4b4d7) took off what the
  // old map lent it: the routes GET /api/yard/routes serves and the live
  // motion over them, both keyed by the old map's regions and placed by
  // the `REGION_AT` table that retired with it, and the four region views.
  // Motion is the station page's (StationLine.svelte). The
  // alarms board moved to the Department Map (MapPage.svelte).
  //
  // THREE READS, EACH SAID WHEN IT FAILS. The tree is the page: a failed
  // tree read draws nothing and says why. The load (GET
  // /api/stations/load) and the open claims (GET /api/claims: what each
  // card's pools hold in use, design c4854fd4 E1) ride on it: each that
  // fails is named beside the floor, and the floor still draws — a card a
  // failed read feeds keeps the red rail and draws no figure from it.
  // With no station asked for, the root is found in GET /api/stations by
  // the tree's shape — the one tree row with no parent — never by name.
  import { onMount } from 'svelte';
  import { navigate } from '../../router';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { parseStationLoad, type StationLoadRow } from '../marshalling/marshalling';
  import StationCard from './StationCard.svelte';
  import { measureWidth } from './measure-width';
  import { safeLinkHref } from '@boss/web-kit/links';
  import {
    DEPTH_CHOICES, claimsPath, parseClaims, parseTree, rootIdOf, stationCard, treeHref,
    type CardReads, type ClaimsRead, type StationTree,
  } from './station-tree';
  import { layoutTree, treeEdges, type TreeLayout } from './tree-layout';

  type Props = Readonly<{ under: string | null; depth: number }>;
  let { under, depth }: Props = $props();

  type Read<T> = Remote<T>;
  const LOADING = { kind: 'loading' } as const;
  let tree = $state.raw<Read<StationTree>>(LOADING);
  let load = $state.raw<Read<ReadonlyArray<StationLoadRow>>>(LOADING);
  let claims = $state.raw<Read<ClaimsRead>>(LOADING);
  let width = $state(1200);

  async function readTree(asked: string | null): Promise<Exclude<Read<StationTree>, { kind: 'loading' }>> {
    let id = asked;
    if (id === null) {
      const list = await fetchRemote('/api/stations', (raw: unknown) => raw);
      if (list.kind === 'failed') return list;
      id = rootIdOf(list.data);
      if (id === null) {
        return { kind: 'failed', error: 'the station registry has no single root: no tree row, or more than one, names no parent' };
      }
    }
    return fetchRemote(`/api/stations?under=${encodeURIComponent(id)}`, parseTree);
  }

  // The claims are the floor's own subtree's (backlog d05e29a7), read
  // beside the tree and again when another station is asked. With none
  // asked the floor stands at the root, whose subtree is the company:
  // the server answers every claim to the root and to no station alike,
  // so the read does not wait on the registry to name the root first.
  $effect(() => {
    const asked = under;
    let gone = false;
    tree = LOADING;
    claims = LOADING;
    void readTree(asked).then((t) => {
      if (!gone) tree = t;
    });
    void fetchRemote(claimsPath(asked), parseClaims).then((r) => {
      if (!gone) claims = r;
    });
    return () => {
      gone = true;
    };
  });

  onMount(() => {
    let gone = false;
    void fetchRemote('/api/stations/load', parseStationLoad).then((r) => {
      if (!gone) load = r;
    });
    return () => {
      gone = true;
    };
  });

  /** What every card draws beside the tree, as the reads stand. */
  const reads = $derived<CardReads>({
    load: load.kind === 'ready' ? { kind: 'ready', data: new Map(load.data.map((r) => [r.station, r] as const)) } : load,
    claims,
  });

  function lay(t: StationTree, r: CardReads, w: number, d: number): TreeLayout {
    return layoutTree(t, { width: w, depth: d, lines: (n) => stationCard(t, n, r)?.lines ?? 0 });
  }
  const floor = $derived.by(() => {
    if (tree.kind !== 'ready') return null;
    const t = tree.data;
    const layout = lay(t, reads, width, depth);
    return { t, layout, lines: treeEdges(t, layout) };
  });
  /** The address the read derived, one link per station on it. */
  const segments = $derived(floor === null ? [] : (floor.t.byName.get(floor.t.top)?.address ?? '').split('/').filter((s) => s !== ''));

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

<section class="tree-page" data-tree-page>
  <header class="band">
    <h1 class="title">Station tree</h1>
    {#if floor !== null}
      <span class="address" data-tree-address>{#each segments as s, i (i)}/<a href={safeLinkHref(treeHref(s, depth))} onclick={(e) => go(e, treeHref(s, depth))}>{s}</a>{/each}</span>
    {/if}
    <span class="grow"></span>
    <nav class="depth" aria-label="How many levels to draw">
      <span class="eyebrow">depth</span>
      {#each DEPTH_CHOICES as d (d)}
        {@const to = treeHref(under, d)}
        <a href={safeLinkHref(to)} onclick={(e) => go(e, to)} data-depth-choice={d} aria-current={d === depth ? 'true' : undefined}>{d}</a>
      {/each}
    </nav>
  </header>
  <p class="note">
    One card, drawn recursively: every box is a station — its department's rail, one plate, the pools it holds
    a cell per unit (filled in use, open free) and its parts. Lines are the station rows' edges. Open a station
    for its page, where the work moving through it is drawn.
  </p>

  {#if tree.kind === 'loading'}
    <p role="status" data-tree-loading>Reading the station tree…</p>
  {:else if tree.kind === 'failed'}
    <p class="load-failed" data-tree-failed>The station tree could not be read, so nothing is drawn: {tree.error}</p>
  {:else if floor !== null}
    <div class="scroll" use:measureWidth={(w) => (width = w)}>
      <div class="floor" data-tree-floor style="width:{floor.layout.width}px; height:{floor.layout.height}px">
        <StationCard tree={floor.t} layout={floor.layout} name={floor.t.top} {reads} originX={0} originY={0} />
        <svg class="over" width={floor.layout.width} height={floor.layout.height}
          viewBox="0 0 {floor.layout.width} {floor.layout.height}" aria-hidden="true">
          {#each floor.lines as r (r.key)}
            <path d={r.d} class="route" data-tree-route={r.key}>
              <title>{r.from} → {r.to} · a station row declares this edge</title>
            </path>
          {/each}
          {#each floor.layout.boxes as b (b.name)}
            {@const p = floor.layout.ring(b.name)}
            {#if p !== null}
              <circle cx={p.x} cy={p.y} r="5" class="ring" data-ring={b.name} />
            {/if}
          {/each}
        </svg>
      </div>
    </div>
    <section class="gaps" data-tree-gaps aria-label="What the reads could not say">
      <h2 class="eyebrow">What the reads could not say</h2>
      <ul>
        {#each floor.t.gaps as g (g)}<li>{g}</li>{/each}
        {#each floor.layout.missing as m (m)}<li>the read names <code>{m}</code> as a child but does not carry it</li>{/each}
        {#each floor.layout.unreached as u (u)}<li><code>{u}</code> is in the read but no station of it contains it</li>{/each}
        {#if load.kind === 'failed'}<li class="load-failed" data-load-failed>the load read failed, so no card shows a packet standing or judges its limit: {load.error}</li>{/if}
        {#if claims.kind === 'failed'}<li class="load-failed" data-claims-failed>the claims read failed, so no card shows what its pools hold in use or how many seats are busy: {claims.error}</li>{/if}
      </ul>
    </section>
  {/if}
</section>

<style>
  .tree-page { background: var(--map-bg); color: var(--map-ink); }
  .band { display: flex; align-items: center; gap: var(--s3); padding: var(--s2) 0; border-bottom: 1px solid var(--map-rule); flex-wrap: wrap; }
  .title { font-size: 15px; margin: 0; }
  .address { font-family: var(--font-mono); font-size: 12px; letter-spacing: 0.06em; color: var(--map-muted); }
  .address a { color: var(--map-link); text-decoration: none; }
  .address a:hover { text-decoration: underline; }
  .grow { flex: 1; }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.3em; font-size: 10px; color: var(--map-muted); margin: 0; font-weight: 400; }
  .depth { display: flex; align-items: center; gap: var(--s1); }
  .depth a { font-family: var(--font-mono); font-size: 11px; padding: 2px 8px; border: 1.5px solid var(--map-rule-strong); border-radius: 4px;
    color: var(--map-ink); text-decoration: none; }
  .depth a[aria-current='true'] { background: var(--map-ink); color: var(--map-surface); border-color: var(--map-ink); }
  .note { font-size: 12px; color: var(--map-muted); margin: var(--s2) 0; }
  /* The floor is as wide as its layout; it scrolls in its own frame and
     lends the page none of its width (contain), so /it never scrolls sideways. */
  .scroll { overflow: auto; padding-bottom: var(--s2); contain: inline-size; }
  .floor { position: relative; }
  .over { position: absolute; left: 0; top: 0; overflow: visible; pointer-events: none; }
  .route { fill: none; stroke: var(--map-line-delivery); stroke-width: 3; stroke-linecap: round; stroke-linejoin: round; opacity: 0.55; }
  .ring { fill: var(--map-surface); stroke: var(--map-ink); stroke-width: 2.5; }
  .gaps { margin-top: var(--s3); font-size: 12px; color: var(--map-muted); }
  .gaps ul { margin: var(--s1) 0; padding-left: var(--s4); }
</style>
