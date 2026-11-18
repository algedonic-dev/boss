<script lang="ts">
  // THE RIGHT RAIL of a station page (design 08d1530e, car A, decided
  // items 6 and 7; backlog 950ad972; E2 on the design's review step is
  // the look): the waits ladder worst first, the pools as one strip each
  // with the caveat said once (David: "put the station's resource pools
  // to the right so the active lines between stations is the focus"),
  // Stuck, Needs you, and the moves this session saw. Every figure is
  // station-answer.ts's or station-page.ts's; this file only draws.
  //
  // THE MOVES are the recorded moves stream (GET /api/yard/moves/stream),
  // of the protocols the station's own stations queue. Nothing replays:
  // a page opened after a move never lists it, and a stream the browser
  // gave up says so. StationLine.svelte reads the same stream for the
  // line and its list of moves, so this one draws only the rail.
  import { onMount } from 'svelte';
  import { navigate } from '../../router';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { parseMove, parseResync } from './move-wire';
  import { admitRailMove, type HeldKinds, type Ladder, type NeedsList, type RailMove, type Stuck } from './station-answer';
  import type { PoolBand } from './station-page';
  import type { Moving } from './station-motion';

  type Props = Readonly<{
    name: string;
    ladder: Ladder;
    bands: ReadonlyArray<PoolBand>;
    caveat: string | null;
    moving: Moving | null;
    stuck: Stuck;
    needs: NeedsList;
    kinds: HeldKinds;
  }>;
  let { name, ladder, bands, caveat, moving, stuck, needs, kinds }: Props = $props();

  /** How many rungs the ladder draws before it counts the rest. */
  const RUNGS = 6;

  const qty = (n: number): string => String(Math.round(n * 100) / 100);
  /** A strip's figure: in use of what it holds when the claims record it,
   *  else what it holds; a capacity no row states is said so. */
  function figure(b: PoolBand): string {
    const unit = b.unit === '' ? '' : ` ${b.unit}`;
    const own = b.segments.filter((s) => !s.hatched);
    const held = own.every((s) => s.amount !== null) ? own.reduce((a, s) => a + (s.amount ?? 0), 0) : null;
    if (held === null) return 'share unknown';
    const used = own.every((s) => s.inUse !== null) ? own.reduce((a, s) => a + (s.inUse ?? 0), 0) : null;
    return used === null ? `${qty(held)}${unit}` : `${qty(used)} / ${qty(held)}${unit}`;
  }
  const total = (b: PoolBand): number => b.segments.reduce((a, s) => a + Math.max(s.amount ?? 0, 0), 0);

  let feed = $state<'connecting' | 'live' | 'down'>('connecting');
  let moves = $state.raw<ReadonlyArray<RailMove>>([]);

  onMount(() => {
    let seq: number | null = null;
    const es = new EventSource('/api/yard/moves/stream');
    es.addEventListener('open', () => { feed = 'live'; });
    es.addEventListener('resync', (e) => {
      const r = parseResync((e as MessageEvent<string>).data);
      if (r !== null) seq = r.seq;
      feed = 'live';
    });
    es.addEventListener('move', (e) => {
      const mv = parseMove((e as MessageEvent<string>).data);
      if (mv === null || (seq !== null && mv.seq <= seq)) return;
      seq = mv.seq;
      moves = admitRailMove(moves, mv, kinds);
    });
    es.addEventListener('error', () => {
      // A stream the browser is retrying stays as it was; only one it
      // gave up is down.
      if (es.readyState !== EventSource.CLOSED) return;
      feed = 'down';
    });
    return () => es.close();
  });

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

<aside class="rail-col" aria-label="What {name} waits on, holds, and needs" data-station-rail={name}>
  <section id="waits" data-rail-section="waits" aria-labelledby="h-waits">
    <h2 id="h-waits">Waits <span class="muted">not admitted, worst first</span></h2>
    {#if ladder.kind === 'unread'}
      <p class="load-failed" data-waits-unread>{ladder.why}</p>
    {:else}
      {#each ladder.rungs.slice(0, RUNGS) as r (r.station)}
        <div class="rung" class:stuck={r.state === 'stuck'} data-rung={r.station} data-rung-state={r.state}>
          <a class="k" href={safeLinkHref(r.href)} onclick={(e) => go(e, r.href)}>{r.station}</a>
          <span class="n num" data-rung-figure>{r.notAdmitted ?? r.waiting}{r.oldest === null ? '' : ` · ${r.oldest}`}</span>
          <span class="d">{r.words}</span>
        </div>
      {:else}
        <p class="none" data-no-waits>Nothing waits at {name} or below it.</p>
      {/each}
      {#if ladder.rungs.length > RUNGS}<p class="more" data-more="waits">+{ladder.rungs.length - RUNGS} more</p>{/if}
      <p class="note" data-waits-caveat>{ladder.caveat}</p>
    {/if}
  </section>

  <section data-rail-section="pools" aria-labelledby="h-pools">
    <h2 id="h-pools">What {name} holds</h2>
    {#each bands as b (b.pool)}
      {@const all = total(b)}
      <div class="pool" class:moving={moving !== null && moving.pools.includes(b.pool)} data-pool-band={b.pool}>
        <b>{b.pool}</b>
        <span class="f num" data-pool-figure>{figure(b)}</span>
        {#if b.segments.length > 0 && all > 0}
          <div class="pbar" aria-hidden="true">
            {#each b.segments as s (s.key)}
              {#if (s.amount ?? 0) > 0}
                <i class="seg" class:hatched={s.hatched} style="width:{((s.amount ?? 0) / all) * 100}%" data-segment={s.key}>
                  {#if s.inUse !== null && s.amount !== null && s.amount > 0}<u style="width:{Math.min(100, (s.inUse / s.amount) * 100)}%"></u>{/if}
                </i>
              {/if}
            {/each}
          </div>
          <span class="s" data-segment-labels>{b.segments.map((s) => `${s.label} ${s.amount === null ? '?' : qty(s.amount)}`).join(' · ')}</span>
        {/if}
        {#if b.host !== null}<span class="s">{b.host}</span>{/if}
        {#if b.notes.length > 0}<span class="s anomaly" data-pool-notes>{b.notes.join(' · ')}</span>{/if}
        {#if moving !== null && moving.pools.includes(b.pool)}<span class="s moving-note" data-band-moving>moving with {moving.label}: {moving.words}</span>{/if}
      </div>
    {:else}
      <p class="none" data-no-pools>{name} holds no pool and draws on none the claims record.</p>
    {/each}
    {#if caveat !== null}<p class="note" data-pools-caveat>{caveat}</p>{/if}
  </section>

  <section id="stuck" data-rail-section="stuck" aria-labelledby="h-stuck">
    <h2 id="h-stuck">Stuck</h2>
    {#if stuck.kind === 'unread'}
      <p class="load-failed" data-stuck-unread>{stuck.why}</p>
    {:else}
      <ul class="list">
        {#each stuck.items as i (i.key)}
          <li data-stuck={i.key}>
            {#if i.href !== null}<a href={safeLinkHref(i.href)} onclick={(e) => go(e, i.href!)}>{i.words}</a>{:else}<span>{i.words}</span>{/if}
            <span class="num">{i.age ?? ''}</span>
          </li>
        {:else}
          <li class="none" data-nothing-stuck>Nothing is judged stuck.</li>
        {/each}
      </ul>
    {/if}
  </section>

  <section id="needs-you" data-rail-section="needs-you" aria-labelledby="h-needs">
    <h2 id="h-needs">Needs you <span class="muted">human-required steps</span></h2>
    {#if needs.kind === 'unread'}
      <!-- Unread in quiet ink, not the failure rail: a server that does
           not answer the canonical human-required view (no
           human_required_view: 1) cannot be read as "nothing needs you",
           and it is not this page failing either. It is said the way
           IT's own board says it (NeedsYou.svelte), beside a ? on the band. -->
      <p class="note" data-needs-unread>not read: {needs.why}</p>
    {:else}
      <ul class="list">
        {#each needs.lines as l (l.id)}
          <li data-need={l.id}><a href={safeLinkHref(l.href)}>{l.title}</a><span class="num">{l.age}</span><span class="s">{l.step}</span></li>
        {:else}
          <li class="none" data-no-needs>No step here waits on a person.</li>
        {/each}
        {#if needs.more > 0}<li class="more" data-more="needs">+{needs.more} more</li>{/if}
      </ul>
      {#if needs.cut !== null}<p class="note" data-needs-cut>{needs.cut}</p>{/if}
    {/if}
  </section>

  <section data-rail-section="moves" aria-labelledby="h-moves">
    <h2 id="h-moves">Moves <span class="muted">from the record, this visit, newest first</span></h2>
    {#if feed === 'down'}
      <p class="load-failed" data-moves-down>The moves stream is down (the browser gave it up), so no move is listed until it answers.</p>
    {:else if kinds.kind === 'unread' || kinds.kind === 'none'}
      <p class="none" data-moves-unscoped>No move is listed: {kinds.why}.</p>
    {:else}
      <ul class="mv">
        {#each moves as m (m.seq)}
          <li data-rail-move={m.seq}><span class="num">{m.at}</span><a href={safeLinkHref(m.href)}>{m.label}</a><span class="s">{m.route} · {m.cause}</span></li>
        {:else}
          <li class="none" data-rail-no-moves>{feed === 'live' ? 'No move recorded since this page opened.' : 'Connecting to the moves stream…'}</li>
        {/each}
      </ul>
      <p class="note">The record still names the retired regions a move ran between, not stations.</p>
    {/if}
  </section>
</aside>

<style>
  /* E2's rail (design 08d1530e): a night-ink rule over each section,
     hairline rows, mono figures. */
  .rail-col { min-width: 0; }
  section { border-top: 2px solid var(--fog); padding-top: 7px; margin-bottom: var(--s3); }
  h2 { font-size: 14px; font-weight: 800; margin: 0 0 6px; display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
  .muted { font-weight: 400; font-size: 12px; color: var(--static); }
  .num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  a { color: var(--signal); text-decoration: none; }
  a:hover { text-decoration: underline; }
  .note { font-size: 11px; color: var(--static); margin: 4px 0 0; }
  .none { font-size: 12px; color: var(--static); margin: var(--s1) 0; }
  .more { font-size: 12px; color: var(--static); margin: 4px 0 0; }
  /* An unread figure wears the global .load-failed rail (styles.css,
     backlog 6f471ff6); no rule here repaints it. */

  .rung { display: grid; grid-template-columns: 1fr auto; gap: 1px 8px; padding: 4px 0; border-bottom: 1px solid var(--hairline); }
  .rung .k { font-weight: 700; font-size: 12px; color: var(--fog); }
  .rung .n { font-size: 11.5px; text-align: right; }
  .rung .d { grid-column: 1 / 3; font-size: 11px; color: var(--static); }
  .rung.stuck .n { color: var(--troubled-ink); font-weight: 600; }

  .pool { display: grid; grid-template-columns: 1fr auto; gap: 2px 8px; padding: 4px 0; border-bottom: 1px solid var(--hairline); font-size: 12px; }
  .pool .f { font-size: 11px; color: var(--static); }
  .pool .s { grid-column: 1 / 3; font-size: 10.5px; color: var(--static); }
  .pool .anomaly { color: var(--troubled-ink); }
  .pool .moving-note { color: var(--fog); font-weight: 600; }
  .pool.moving .pbar { outline: 2px solid var(--signal); outline-offset: 2px; }
  .pbar { grid-column: 1 / 3; display: flex; height: 9px; border: 1.5px solid var(--fog); border-radius: 2px; overflow: hidden; background: var(--void); }
  .pbar .seg { position: relative; display: block; height: 100%; border-right: 1.5px solid var(--void); background: color-mix(in srgb, var(--line, var(--signal)) 28%, var(--void)); }
  .pbar .seg:last-child { border-right: 0; }
  .pbar .seg u { position: absolute; inset: 0 auto 0 0; background: var(--fog); text-decoration: none; }
  /* Someone else's share: hatched in static ink. */
  .pbar .seg.hatched { background: repeating-linear-gradient(135deg, var(--static) 0 1.5px, transparent 1.5px 4px); }

  .list, .mv { list-style: none; margin: 0; padding: 0; }
  .list li { display: grid; grid-template-columns: 1fr auto; gap: 0 8px; padding: 4px 0; border-bottom: 1px solid var(--hairline); font-size: 12px; }
  .list li a { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .list li .s { grid-column: 1 / 3; font-size: 10.5px; color: var(--static); }
  .mv li { display: grid; grid-template-columns: 64px 1fr; gap: 0 8px; padding: 4px 0; border-bottom: 1px solid var(--hairline); font-size: 11.5px; }
  .mv li .num { font-size: 11px; color: var(--static); }
  .mv li a { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .mv li .s { grid-column: 2; color: var(--static); font-size: 10px; font-family: var(--font-mono); }
  .mv li.none { display: block; }
</style>
