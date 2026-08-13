<script lang="ts">
  // /system/map — the network's stations as a live map, v1
  // (docs/design/stations.md; the canvas decisions are folded into
  // docs/architecture-decisions.md §Step UX & frontend). Every node is
  // an active registry row; depth is the station's evaluated queue,
  // polled; clicking a node opens that queue in the yard's packet-card
  // grammar. Reads only — guest-safe by construction.
  //
  // Layout judgment call: v1 has NO edges (routes need the evented-
  // motion follow-up), and a graph canvas without edges is a grid
  // wearing a dependency — panning, zooming and drag add nothing to a
  // handful of static tiles. So the nodes render as a plain CSS grid
  // in the yard's dock idiom; @xyflow/svelte joins when the router's
  // arrival/departure markers give it edges to draw.
  import { onMount } from 'svelte';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import {
    fetchStations,
    fetchQueue,
    withDepth,
    type MapState,
    type StationQueueView,
  } from './stationMap';

  let map = $state<MapState>({ kind: 'loading' });
  let selected = $state<string | null>(null);
  let queue = $state<StationQueueView | null>(null);
  let queueLoading = $state(false);

  // One poll updates every node's depth (and the open queue panel, so
  // the lens never goes stale while the tiles tick).
  async function pollDepths(): Promise<void> {
    if (map.kind !== 'ready' || map.nodes.length === 0) return;
    const views = await Promise.all(map.nodes.map(n => fetchQueue(n.name)));
    if (map.kind !== 'ready') return;
    map = {
      kind: 'ready',
      nodes: map.nodes.map((n, i) => {
        const v = views[i];
        return v ? withDepth(n, { total: v.total, over_limit: v.overLimit }) : n;
      }),
    };
    if (selected !== null) {
      const idx = map.nodes.findIndex(n => n.name === selected);
      const v = idx >= 0 ? (views[idx] ?? null) : null;
      if (v) queue = v;
    }
  }

  onMount(() => {
    let cancelled = false;
    async function load(): Promise<void> {
      const s = await fetchStations();
      if (cancelled) return;
      map = s;
      await pollDepths();
    }
    load();
    const t = setInterval(() => {
      if (!cancelled) pollDepths();
    }, 15_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  async function select(name: string): Promise<void> {
    if (selected === name) return;
    selected = name;
    queue = null;
    queueLoading = true;
    const v = await fetchQueue(name);
    if (selected === name) {
      queue = v;
      queueLoading = false;
    }
  }

  function onTileKeydown(e: KeyboardEvent, name: string): void {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      select(name);
    }
  }
</script>

<div class="theme-exec map-root">
  <PageHeader
    eyebrow="IT · Network"
    title="Network map"
    subtitle="Every station in the registry — the nodes that hold and route job-packet traffic"
  />

  {#if map.kind === 'loading'}
    <div class="map-empty">Reading the network…</div>
  {:else if map.kind === 'unavailable'}
    <div class="map-empty">
      The station registry hasn't reached this deployment yet — the map lights up
      when it lands.
    </div>
  {:else if map.kind === 'error'}
    <div class="map-empty">The network map is unreachable right now.</div>
  {:else}
    <div class="map-section">01 — STATIONS <span class="map-n">{map.nodes.length}</span></div>
    {#if map.nodes.length === 0}
      <div class="map-empty">The registry holds no active stations yet.</div>
    {:else}
      <div class="map-grid">
        {#each map.nodes as n (n.name)}
          <div
            class="map-node"
            class:selected={selected === n.name}
            style="--pk: {n.hue}"
            role="button"
            tabindex="0"
            title="{n.title} — open this station's queue"
            onclick={() => select(n.name)}
            onkeydown={e => onTileKeydown(e, n.name)}
          >
            <div class="map-nodehead">
              <span class="map-title">{n.title}</span>
              <span class="map-kind">{n.kind}</span>
            </div>
            <div class="map-depthrow">
              <span class="map-depth" class:over={n.overLimit}>
                {n.depth === null ? '—' : n.depth}
              </span>
              <span class="map-depthword">{n.depth === 1 ? 'packet' : 'packets'}</span>
              {#if n.overLimit}
                <span class="map-over">OVER LIMIT{n.wipLimit === null ? '' : ` (${n.wipLimit})`}</span>
              {/if}
            </div>
            <div class="map-discipline">{n.discipline}</div>
          </div>
        {/each}
      </div>

      {#if selected !== null}
        <div class="map-section">
          02 — QUEUE · {selected}
          {#if queue}
            <span class="map-n">{queue.total}</span>
            <span class="map-disc-chip">{queue.discipline}</span>
            {#if queue.overLimit}<span class="map-over">OVER LIMIT</span>{/if}
          {/if}
        </div>
        {#if queueLoading}
          <div class="map-empty">Evaluating the queue…</div>
        {:else if !queue}
          <div class="map-empty">This station's queue is unreachable right now.</div>
        {:else if queue.cards.length === 0}
          <div class="map-empty">The queue is clear.</div>
        {:else}
          <div class="map-queue">
            {#each queue.cards as c (c.id)}
              <PacketCard card={c} size="dock" />
            {/each}
          </div>
        {/if}
      {/if}
    {/if}

    <div class="map-flow">NODES TODAY — ROUTES ARRIVE WITH MOTION EVENTS</div>
  {/if}
</div>

<style>
  .map-root { padding: 0 32px 32px; }
  .map-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5FD4A8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
    text-transform: uppercase;
  }
  .map-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2A3138); }
  .map-n { color: var(--static, #7A838C); }
  .map-grid { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); }
  .map-node {
    background: var(--card, var(--ink, #12161C));
    border: 1px solid var(--hairline, #2A3138);
    border-top: 3px solid var(--pk);
    padding: 10px 12px;
    cursor: pointer;
    transition: border-color 120ms ease;
  }
  .map-node:hover { border-color: var(--pk); }
  .map-node:focus-visible { outline: 1px solid var(--pk); outline-offset: 2px; }
  .map-node.selected { border-color: var(--signal, #5FD4A8); }
  .map-nodehead { display: flex; align-items: baseline; gap: 10px; }
  .map-title {
    flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
  }
  .map-kind {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--pk); border: 1px solid var(--pk); padding: 0 5px; flex: none;
  }
  .map-depthrow { display: flex; align-items: baseline; gap: 6px; margin: 8px 0 6px; }
  .map-depth { font-size: 22px; line-height: 1; }
  .map-depth.over { color: var(--warn, #d9a441); }
  .map-depthword { font-size: 12px; color: var(--static, #7A838C); }
  .map-over {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px; letter-spacing: 0.1em;
    color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 0 5px; margin-left: auto;
  }
  .map-discipline {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; color: var(--static, #7A838C);
  }
  .map-disc-chip {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px; letter-spacing: 0.1em; text-transform: none;
    color: var(--static, #7A838C); border: 1px solid var(--hairline, #2A3138);
    padding: 0 5px;
  }
  .map-queue { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr)); }
  .map-empty { color: var(--static, #78716c); padding: 12px 0; font-size: 14px; }
  .map-flow {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: var(--ls-nav, 0.14em);
    color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px;
  }
</style>
