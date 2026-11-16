<script lang="ts">
  // ONE STATION CARD, DRAWN RECURSIVELY — the five-row card of design
  // c4854fd4 E1 (decided by David 2026-11-07 23:38Z; backlog e1b13a15),
  // the unit a parent floor tiles. The same rows on every station kind,
  // an empty one saying so, so tiles line up:
  //
  //   1 Rail   the department's line (identity, never state)
  //   2 Band   name, mode as the frame, one plate that carries its word
  //   3 Pools  one cell per unit: filled in use, open free
  //   4 Parts  sub-stations, ad hoc open, seats, machines, standing
  //   5 Nothing else — no packets, no movement, no connections
  //
  // Every figure is station-tree.ts `stationCard`'s, from the real,
  // current reads (the station rows, the open-packet load, the open
  // claims); nothing here decides a number or a place. Each body line is
  // CARD.line tall, so the card is exactly as tall as the layout counted.
  //
  // Positioned absolutely inside the parent card, at the layout's own
  // coordinates less the parent's, so the DOM nests exactly as the tree
  // does and the rings drawn over it (TreeFloor.svelte) land on the
  // cards they name.
  import { navigate } from '../../router';
  import { partWords, poolFigure, poolFill, stationCard, type CardReads, type StationTree } from './station-tree';
  import { stationHref } from './station-page';
  import { CARD, type TreeLayout } from './tree-layout';
  import StationCard from './StationCard.svelte';
  import { safeLinkHref } from '@boss/web-kit/links';

  type Props = Readonly<{
    tree: StationTree;
    layout: TreeLayout;
    name: string;
    reads: CardReads;
    /** The parent card's corner, in floor coordinates. */
    originX: number;
    originY: number;
  }>;
  let { tree, layout, name, reads, originX, originY }: Props = $props();

  const node = $derived(tree.byName.get(name));
  const box = $derived(layout.boxes.find((b) => b.name === name));
  const model = $derived(stationCard(tree, name, reads));
  const kids = $derived(layout.boxes.filter((b) => b.parent === name).map((b) => b.name));
  /** The rail's colour: a department line token, else night ink. */
  const line = $derived(
    model === null || model.rail.line === null ? 'var(--fog)' : `var(--map-line-${model.rail.line}, var(--fog))`,
  );

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

{#if node !== undefined && box !== undefined && model !== null}
  <!-- Opening a station opens its page (design c4854fd4 E2): the default
       when a station is opened, a view swap rather than a zoom. -->
  {@const to = stationHref(name)}
  <article class="st" class:adhoc={model.mode === 'ad-hoc'} class:troubled={model.plate.state === 'troubled'}
    data-station={name} data-address={node.address} data-rail={model.rail.mark} data-line={model.rail.line ?? 'none'}
    style="--line:{line}; --row:{CARD.line}px; left:{box.x - originX}px; top:{box.y - originY}px; width:{box.w}px; height:{box.h}px">
    <span class="rail {model.rail.mark}" aria-hidden="true"></span>
    <header class="band" title={node.title}>
      <a class="name" href={safeLinkHref(to)} onclick={(e) => go(e, to)}>{name}</a>
      <span class="plate {model.plate.state}" data-plate={model.plate.state} title={model.plate.why ?? undefined}>{model.plate.word}</span>
    </header>
    <div class="body" data-card-body>
      <div class="row pools" data-card-row="pools">
        {#each model.pools as g (g.group)}
          {#if g.pools.length === 0}
            <div class="line" data-pool-group={g.group}><span class="k">{g.group}</span><span class="v">none held</span></div>
          {:else}
            {#each g.pools as p (p.pool)}
              {@const figure = poolFigure(p)}
              <div class="line" data-pool={p.pool}
                title="{p.pool}{p.host === null ? '' : ` · ${p.host}`}: {figure}{p.inUseNote === null ? '' : ` · ${p.inUseNote}`}{p.inUseCounts === null ? '' : ` (${p.inUseCounts})`}">
                <span class="k strong">{p.pool}</span><span class="v"><b>{figure}</b></span>
              </div>
              {#if p.draw === 'cells' && p.held !== null}
                <div class="line cells" class:unread={p.inUseNote !== null && p.inUseNote !== 'in use not recorded'} data-cells={p.pool}>
                  {#each Array.from({ length: p.held }) as _, i (i)}<span class="c" class:u={p.inUse !== null && i < p.inUse}></span>{/each}
                </div>
              {:else if p.draw === 'bar'}
                <div class="line"><span class="bar" data-bar><i style="width:{(poolFill(p) * 100).toFixed(1)}%"></i></span></div>
              {/if}
            {/each}
          {/if}
        {/each}
        {#if model.poolsNote !== null}
          <div class="line note" data-pools-note title={model.poolsNote}><span class="v">{model.poolsNote}</span></div>
        {/if}
      </div>
      <div class="row parts" data-card-row="parts">
        {#each partWords(model.parts) as p (p.key)}
          <div class="line" data-part={p.key}><span class="k">{p.label}</span><span class="v"><b>{p.value}</b></span></div>
        {/each}
      </div>
    </div>
    {#each kids as k (k)}
      <StationCard {tree} {layout} name={k} {reads} originX={box.x} originY={box.y} />
    {/each}
  </article>
{/if}

<style>
  /* An outline, not a border: a border would move every child card (and
     so every ring drawn over it) in by its width, once per level. Enamel:
     night-ink frames, 4px corners, light only. */
  .st { position: absolute; box-sizing: border-box; background: var(--ink);
    outline: 2px solid var(--fog); outline-offset: -2px; border-radius: 4px; }
  .st.adhoc { outline-style: dashed; outline-color: var(--static); }
  .st.troubled { outline-color: var(--troubled); box-shadow: 0 0 0 2px var(--err-wash); }
  /* 1 Rail: the department's line, the card's full height. Hollow when
     the claims read says its source is gone; red when a read failed. */
  .rail { position: absolute; left: 0; top: 0; bottom: 0; width: 6px; background: var(--line); border-radius: 4px 0 0 4px; }
  .rail.hollow { background: var(--ink); box-shadow: inset 0 0 0 2px var(--line); }
  .rail.failed { background: var(--troubled); }
  /* 2 Band: an enamel band; ad hoc is the pale band of a dashed frame. */
  .band { box-sizing: border-box; height: 28px; display: flex; align-items: center; gap: 8px; padding: 0 8px 0 24px;
    background: var(--band); color: var(--on-band); border-radius: 4px 4px 0 0; white-space: nowrap; overflow: hidden; }
  .st.adhoc .band { background: var(--ink-raised); color: var(--fog); }
  .name { font-weight: 800; font-size: 13px; color: inherit; text-decoration: none; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .name:hover { text-decoration: underline; }
  .plate { margin-left: auto; flex: none; font-family: var(--font-mono); font-weight: 600; font-size: 10px; line-height: 1;
    letter-spacing: 0.1em; text-transform: uppercase; padding: 4px 7px; border-radius: 3px; }
  .plate.clear { background: var(--clear); color: var(--on-clear); }
  .plate.troubled { background: var(--troubled); color: var(--on-troubled); }
  .plate.failed { background: var(--ink); color: var(--troubled-ink); outline: 1.5px solid var(--troubled); outline-offset: -1.5px; }
  .plate.stale, .plate.reading, .plate.unjudged { color: var(--on-band-dim); outline: 1.5px dashed var(--band-rule); outline-offset: -1.5px; }
  .st.adhoc .plate.stale, .st.adhoc .plate.reading, .st.adhoc .plate.unjudged { color: var(--static); outline-color: var(--static); }
  /* Rows: every line one CARD.line, each row a half line above and below. */
  .body { padding-left: 6px; }
  .row { box-sizing: border-box; padding: calc(var(--row) / 2) 10px calc(var(--row) / 2) 8px; }
  .row.parts { border-top: 1px solid var(--hairline); padding-top: calc(var(--row) / 2 - 1px); }
  .line { box-sizing: border-box; height: var(--row); display: flex; align-items: center; justify-content: space-between; gap: 8px;
    font-size: 11.5px; color: var(--static); white-space: nowrap; overflow: hidden; }
  .k { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .strong { color: var(--fog); font-weight: 700; }
  .v { flex: none; }
  .v b { color: var(--fog); font-weight: 700; font-variant-numeric: tabular-nums; }
  .note { justify-content: flex-start; font-style: italic; }
  .note .v { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  /* 3 Pools: one cell per unit. Filled is in use; open is free. With in
     use not recorded every cell is open, and the row's note says so. */
  .cells { justify-content: flex-start; gap: 2px; }
  .c { box-sizing: border-box; width: 6px; height: 12px; border: 1.5px solid var(--line); border-radius: 1.5px; background: var(--ink); flex: none; }
  .c.u { background: var(--line); }
  .cells.unread .c { border-style: dashed; border-color: var(--static); }
  .bar { box-sizing: border-box; position: relative; display: block; flex: 1; height: 12px; border: 1.5px solid var(--line);
    border-radius: 2px; overflow: hidden; background: var(--ink); }
  .bar i { position: absolute; inset: 0 auto 0 0; background: var(--line); }
</style>
