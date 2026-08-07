<script lang="ts">
  // /it/os-map — the company as a network of executors.
  //
  // Every other surface renders the WORK: a Job, a step, a queue. This
  // renders the MACHINE — who the processors are and what moves
  // between them. Design and decisions:
  // docs/design/operating-system-view.md.
  //
  // It polls rather than streams. The map is an aggregate over
  // thousands of handoffs, so a per-event stream would redraw
  // constantly without telling anyone more than a periodic recount
  // does; and the endpoint returns in ~80ms warm, which is what makes
  // polling honest rather than lazy. `high_water` is how the page
  // knows whether anything actually advanced between ticks.
  import { onMount, onDestroy } from 'svelte';
  import { SvelteFlow, Background, Controls, MarkerType } from '@xyflow/svelte';
  import type { Node, Edge } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { layout, toEdges, type OsMap } from './osMapToGraph';

  const POLL_MS = 5_000;

  let map = $state<OsMap | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let lastAdvanced = $state<number | null>(null);
  let live = $state(true);
  let timer: ReturnType<typeof setInterval> | null = null;

  let nodes = $state.raw<Node[]>([]);
  let edges = $state.raw<Edge[]>([]);

  async function load(): Promise<void> {
    try {
      const r = await fetch('/api/views/os-map?limit=5000');
      if (!r.ok) throw new Error(`os-map: HTTP ${r.status}`);
      const next: OsMap = await r.json();
      // Only note movement when the log actually advanced — a tick
      // that changed nothing should not look like activity.
      if (map && next.high_water > map.high_water) lastAdvanced = Date.now();
      map = next;
      // The number is handoffs the executor took part in — out, in,
      // and internal — within the current window. Spelled out rather
      // than left bare: an unlabelled figure on a node reads as a
      // headcount or a queue depth just as easily, and the first
      // question asked of this map was what it meant.
      nodes = layout(next.nodes).map((n) => ({
        ...n,
        data: {
          ...n.data,
          label: `${n.data.label}\n${Number(n.data.touched).toLocaleString()} handoffs`,
        },
      }));
      edges = toEdges(next.edges).map((e) => ({
        ...e,
        markerEnd: { type: MarkerType.ArrowClosed },
      }));
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  function setLive(on: boolean): void {
    live = on;
    if (timer) clearInterval(timer);
    timer = null;
    if (on) timer = setInterval(load, POLL_MS);
  }

  onMount(() => {
    void load();
    setLive(true);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  let simulatedShare = $derived.by(() => {
    if (!map || map.handoffs_considered === 0) return 0;
    const sim = map.edges.reduce((n, e) => n + e.simulated, 0);
    return Math.round((sim / map.handoffs_considered) * 100);
  });
</script>

<PageHeader
  title="Operating system"
  subtitle="Every executor that moves work, and what moves between them. Edges are step handoffs; a loop is work moving inside one department."
/>

<div class="os-bar">
  <button class="os-btn" class:os-btn-on={live} type="button" onclick={() => setLive(!live)}>
    {live ? 'Live' : 'Paused'}
  </button>
  {#if map}
    <span class="os-stat">{map.nodes.length} executors</span>
    <span class="os-stat">{map.edges.length} routes</span>
    <span class="os-stat">{map.handoffs_considered.toLocaleString()} handoffs in window</span>
    <span class="os-note">
      A node's count is every handoff it took part in — sent, received, or
      internal — so the counts sum to more than the window: a handoff has two
      ends.
    </span>
    <span class="os-legend"><i class="os-swatch os-swatch-real"></i>real</span>
    <span class="os-legend"><i class="os-swatch os-swatch-sim"></i>simulated ({simulatedShare}%)</span>
  {/if}
</div>

{#if loading}
  <p class="os-msg">Reading the network…</p>
{:else if error}
  <p class="os-msg os-err">{error}</p>
{:else if map && map.edges.length === 0}
  <p class="os-msg">
    Nothing has moved between executors in the last 5,000 step completions. An empty map
    means no handoffs, not a broken read.
  </p>
{:else}
  <div class="os-canvas">
    <SvelteFlow bind:nodes bind:edges fitView>
      <Background />
      <Controls />
    </SvelteFlow>
  </div>
{/if}

<style>
  .os-bar {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 10px;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }
  .os-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 10px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .os-btn-on {
    background: #0f766e;
    border-color: #0f766e;
    color: #fff;
  }
  .os-stat {
    font-variant-numeric: tabular-nums;
  }
  .os-note {
    flex-basis: 100%;
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    max-width: 62ch;
  }
  .os-legend {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .os-swatch {
    width: 16px;
    height: 3px;
    border-radius: 2px;
    display: inline-block;
  }
  .os-swatch-real {
    background: #0f766e;
  }
  .os-swatch-sim {
    background: #a78bfa;
  }
  .os-canvas {
    height: min(72vh, 760px);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    overflow: hidden;
    background: var(--card, #fff);
  }
  .os-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .os-err {
    color: #b91c1c;
  }
  /* The dispatcher is an executor like any other, but it is the one
     everything routes through — distinct enough to find at a glance,
     not so distinct it reads as a different kind of thing. */
  :global(.os-node-dispatcher) {
    border-color: #b45309 !important;
    background: #fffbeb !important;
    font-weight: 600;
  }
  :global(.os-node) {
    font-size: 12px;
    /* The label carries a second line now. */
    white-space: pre-line;
    text-align: center;
    line-height: 1.3;
  }
</style>
