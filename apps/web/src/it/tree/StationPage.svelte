<script lang="ts">
  // THE STATION PAGE — what opening a station shows (design c4854fd4 E2,
  // decided by David 2026-11-07 23:38Z; backlog 72d58e00; re-cut by design
  // 08d1530e, approved by David 2026-11-12 ~02:20Z, car A, backlog
  // 950ad972 — E2 on that design's review step is the look). The header
  // ANSWERS: Working · Stuck · Needs you as linked counts, and the binding
  // constraint. The always-CLEAR plate, STANDING and the title that
  // repeated the name went. The right rail holds the waits ladder, the
  // pools as one strip each, Stuck, Needs you and the moves
  // (StationRail.svelte); the left holds the parts, the line (car B mounts
  // its line body here) and Working. The In and Out columns went: Out was
  // always "no departures read yet", and In read 1.46 MB of queue to show
  // five titles. Every figure is station-page.ts's or station-answer.ts's,
  // from the real, current reads — the tree, the load, the open claims,
  // the human-required view — and nothing here decides a number. Opening
  // a part is a view swap to its own page, never a camera zoom (David
  // 2026-09-20). Motion is StationLine.svelte's (design 08d1530e car B,
  // which replaced car 3's motion band, backlog 87c5293f): while a packet
  // moves the pool strips mark the pools its claims moved in, from a
  // claims read taken when it moved.
  //
  // NO STATION ASKED FOR IS THE ROOT (R3b of the transit-map retirement,
  // backlog 23a4b4d7): /it draws the company station's page, found by the
  // tree's shape — the one tree row with no parent — never by name, so
  // the live registry's `algedonic` and a fixture's `company` read alike.
  // Under another page's header (/it) its heading is an h2.
  //
  // EACH READ IS SAID WHEN IT FAILS. The tree is the page: a failed tree
  // read draws the decided red rail and no figure. A failed claims read
  // keeps what the tree holds, draws no in-use figure, and Working, Stuck
  // and the binding say why; a failed load read leaves the waits unread;
  // a failed human-required read leaves Needs you unread.
  import { navigate } from '../../router';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { parseStationLoad, type StationLoadRow } from '../marshalling/marshalling';
  import { fetchNeedsYou, type NeedsYou } from '../yard/human-needs';
  import { safeLinkHref } from '@boss/web-kit/links';
  import StationCard from './StationCard.svelte';
  import StationRail from './StationRail.svelte';
  import LocalUtc from './LocalUtc.svelte';
  import StationLine from './StationLine.svelte';
  import type { Moving } from './station-motion';
  import { CARD, layoutTree, type TreeLayout } from './tree-layout';
  import { claimsPath, parseTree, rootIdOf, stationCard, treeHref, type CardReads, type StationTree } from './station-tree';
  import {
    HOLDER_MARK, cardReads, parsePageClaims, stationPage,
    type HeldClaim, type PageClaims, type PageReads,
  } from './station-page';
  import {
    answerBand, bindingPlate, heldKinds, needsBand, needsYouList, needsYouUrl, stuckList, subtitleOf, waitsLadder,
    type BandCount, type HeldKinds, type NeedsList,
  } from './station-answer';

  type Props = Readonly<{ station: string | null; embedded?: boolean }>;
  let { station, embedded = false }: Props = $props();

  type Read<T> = Remote<T>;
  const LOADING = { kind: 'loading' } as const;
  let tree = $state.raw<Read<StationTree>>(LOADING);
  let load = $state.raw<Read<ReadonlyMap<string, StationLoadRow>>>(LOADING);
  let claims = $state.raw<Read<PageClaims>>(LOADING);
  let needs = $state.raw<Read<NeedsYou>>(LOADING);
  let moving = $state.raw<Moving | null>(null);
  /** When the reads were taken: the ages the band and rail state. */
  let now = $state(Date.now());

  /** The claims again, as a recorded move leaves them: the band stands as
   *  the record does now. A failed read keeps the last one and says so. */
  async function refreshClaims(): Promise<ReadonlyArray<HeldClaim> | null> {
    const r = await fetchRemote(claimsPath(station), parsePageClaims);
    if (r.kind !== 'ready') return null;
    claims = r;
    return r.data.held;
  }

  /** The whole tree, from its root: the breadcrumb is the parent chain,
   *  and a band's hatched share is the rest of the root's pool. The root
   *  is found by the tree's shape, never by a name. */
  async function readTree(): Promise<Exclude<Read<StationTree>, { kind: 'loading' }>> {
    const list = await fetchRemote('/api/stations', (raw: unknown) => raw);
    if (list.kind === 'failed') return list;
    const id = rootIdOf(list.data);
    if (id === null) return { kind: 'failed', error: 'the station registry has no single root: no tree row, or more than one, names no parent' };
    return fetchRemote(`/api/stations?under=${encodeURIComponent(id)}`, parseTree);
  }

  // Every read is taken again when another station is opened: a swap
  // shows the floor as it stands now, never as it stood at the last one.
  $effect(() => {
    const asked = station;
    let gone = false;
    tree = LOADING;
    load = LOADING;
    claims = LOADING;
    now = Date.now();
    void readTree().then((r) => { if (!gone) tree = r; });
    void fetchRemote('/api/stations/load', (raw) => new Map(parseStationLoad(raw).map((r) => [r.station, r] as const))).then((r) => { if (!gone) load = r; });
    // Its own subtree's claims, never every claim (backlog d05e29a7);
    // the root (no station asked) reads them all.
    void fetchRemote(claimsPath(asked), parsePageClaims).then((r) => { if (!gone) claims = r; });
    return () => { gone = true; };
  });
  /** The station drawn: the one asked for, or the root the tree read names. */
  const name = $derived(station ?? (tree.kind === 'ready' ? tree.data.top : null));

  /** The protocols this station's stations queue: what Needs you and the
   *  moves are scoped to (the root asks for all). */
  const kinds = $derived<HeldKinds>(tree.kind === 'ready' && name !== null ? heldKinds(tree.data, name, load) : { kind: 'unread', why: 'reading the station tree' });
  const needsUrl = $derived(kinds.kind === 'all' || kinds.kind === 'kinds' ? needsYouUrl(kinds) : null);
  // The human-required view, once the protocols are known; again only
  // when they change.
  $effect(() => {
    const url = needsUrl;
    let gone = false;
    needs = LOADING;
    if (url !== null) void fetchNeedsYou(undefined, url).then((r) => { if (!gone) needs = r; });
    return () => { gone = true; };
  });

  const reads = $derived<PageReads>({ load, claims });
  const card = $derived<CardReads>(cardReads({ load, claims }));
  const model = $derived(tree.kind === 'ready' && name !== null ? stationPage(tree.data, name, reads) : null);
  /** The rail: the department's line, red when a read failed. */
  const line = $derived(model === null || model.card.rail.line === null ? 'var(--fog)' : `var(--map-line-${model.card.rail.line}, var(--fog))`);

  const answer = $derived(tree.kind === 'ready' && name !== null ? answerBand(tree.data, name, load, claims, now) : null);
  const binding = $derived(bindingPlate(claims));
  const ladder = $derived(tree.kind === 'ready' && name !== null ? waitsLadder(tree.data, name, load, claims) : null);
  const stuck = $derived(tree.kind === 'ready' && name !== null ? stuckList(tree.data, name, load, claims, now) : null);
  const needsList = $derived<NeedsList>(kinds.kind === 'unread' || kinds.kind === 'none'
    ? { kind: 'unread', why: kinds.why, count: { kind: 'unread', why: kinds.why } }
    : needsYouList(needs, now));
  const subtitle = $derived(model === null ? null : subtitleOf(model.name, model.title));

  /** One part's card, laid out alone: its own box, its children folded. */
  function partLayout(t: StationTree, name: string): TreeLayout {
    const alone: StationTree = { ...t, top: name };
    return layoutTree(alone, { width: CARD.leaf, depth: 0, lines: (n) => stationCard(t, n, card)?.lines ?? 0 });
  }

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

<!-- The anchors are written at each call with a literal in-page href:
     safeLinkHref answers null for a fragment, and a bound href={...} must
     go through it (src/linkSinks.test.ts, backlog 4f1f7698). -->
{#snippet count(key: string, c: BandCount, tone: 'clear' | 'troubled' | 'you')}
    <span class="pl {c.kind === 'unread' ? 'unread' : tone === 'troubled' && c.n === 0 ? 'clear' : tone}" aria-hidden="true"></span>
    {#if c.kind === 'count'}
      <b class="num" data-answer-count>{c.n}{c.floor ? '+' : ''}</b><span class="w">{c.words}</span>
    {:else}
      <b class="num" data-answer-count>?</b><span class="w">{key} unread: {c.why}</span>
    {/if}
{/snippet}

<section class="station-page" data-station-page={name ?? ''}>
  {#if tree.kind === 'loading'}
    <p role="status" data-page-loading>Reading the station tree…</p>
  {:else if tree.kind === 'failed'}
    <div class="failed-read" data-page-failed>
      <span class="rail failed" aria-hidden="true"></span>
      <p>The station tree could not be read, so nothing is drawn: {tree.error}</p>
    </div>
  {:else if model === null}
    <p class="load-failed" data-page-missing>The tree read holds no station named <code>{name}</code>. <a href={safeLinkHref(treeHref(null))} onclick={(e) => go(e, treeHref(null))}>The floor</a> shows every station it holds.</p>
  {:else}
    <nav class="crumbs" aria-label="Where this station stands" data-crumbs>
      {#each model.crumbs as c, i (c.name)}
        {#if i > 0}<span class="sep" aria-hidden="true">›</span>{/if}
        {#if c.current}
          <span class="crumb current" aria-current="page" data-crumb={c.name} data-troubled={c.troubled.length > 0 ? 'true' : undefined}>{c.name}</span>
        {:else}
          <a class="crumb" href={safeLinkHref(c.href)} onclick={(e) => go(e, c.href)} data-crumb={c.name}
            data-troubled={c.troubled.length > 0 ? 'true' : undefined}
            title={c.troubled.length > 0 ? `troubled below: ${c.troubled.join(', ')}` : undefined}>{c.name}</a>
        {/if}
        {#if c.troubled.length > 0 && !c.current}<span class="climb" data-climb={c.name}>▲ {c.troubled.join(', ')}</span>{/if}
      {/each}
      <span class="grow"></span>
      <!-- The walk upstream is the parent's page, derived from the tree
           (backlog a1c0a0ff): no row declares where it leads. -->
      {#if model.upstream !== null}
        {@const up = model.upstream}
        <a class="floor-link" href={safeLinkHref(up.href)} onclick={(e) => go(e, up.href)} data-upstream={up.name}
          title={`Walk upstream to ${up.title}`}>↑ {up.name}</a>
      {/if}
      <a class="floor-link" href={safeLinkHref(treeHref(null))} onclick={(e) => go(e, treeHref(null))}>the whole floor</a>
    </nav>

    <!-- THE ANSWER BAND (design 08d1530e item 1). -->
    <header class="head" style="--line:{line}" data-rail={model.card.rail.mark}>
      <div class="band" class:adhoc={model.card.mode === 'ad-hoc'}>
        <span class="rail {model.card.rail.mark}" aria-hidden="true"></span>
        <div class="id">
          <svelte:element this={embedded ? 'h2' : 'h1'} class="name">{model.name}</svelte:element>
          {#if subtitle !== null}<span class="title" data-subtitle>{subtitle}</span>{/if}
          {#if model.card.mode === 'ad-hoc'}<span class="mode">ad hoc</span>{/if}
        </div>
        {#if answer !== null}
          {@const needBand = needsBand(needsList)}
          <div class="answers" data-answers>
            <a class="ans" href="#working" data-answer="working" data-answer-state={answer.working.kind}>{@render count('working', answer.working, 'clear')}</a>
            <a class="ans" href="#stuck" data-answer="stuck" data-answer-state={answer.stuck.kind}>{@render count('stuck', answer.stuck, 'troubled')}</a>
            <a class="ans" href="#needs-you" data-answer="needs you" data-answer-state={needBand.kind}>{@render count('needs you', needBand, 'you')}</a>
          </div>
        {/if}
        <div class="binding" data-binding={binding.kind}>
          {#if binding.kind === 'pool'}
            <span class="row"><span class="lbl">binding</span><span class="plate att" data-binding-pool>{binding.pool} · {binding.figure}</span></span>
            <span class="num">{binding.refused}</span>
          {:else if binding.kind === 'none'}
            <span class="row"><span class="lbl">binding</span><span class="plate quiet">none</span></span>
            <span class="num">{binding.words}</span>
          {:else}
            <span class="row"><span class="lbl">binding</span><span class="plate unread">unread</span></span>
            <span class="num">{binding.why}</span>
          {/if}
        </div>
      </div>
      {#if model.card.plate.why !== null}<p class="why" data-plate-why>{model.card.plate.why}</p>{/if}
    </header>

    <div class="layout">
      <div class="main">
        <!-- PARTS: each sub-station its own E1 card (ad hoc dashed), and
             the station's own machines. -->
        <section class="sect" data-band="parts" aria-labelledby="h-parts">
          <h2 id="h-parts" class="eyebrow">Parts <span class="sub">{model.parts.stations.length} station{model.parts.stations.length === 1 ? '' : 's'}, {model.parts.machines.length} machine{model.parts.machines.length === 1 ? '' : 's'}</span></h2>
          <div class="parts">
            {#each model.parts.stations as p (p)}
              {@const lay = partLayout(tree.data, p)}
              {@const box = lay.boxes[0]}
              {#if box !== undefined}
                <div class="part" data-part-card={p} style="width:{box.w}px; height:{box.h}px">
                  <StationCard tree={tree.data} layout={lay} name={p} reads={card} originX={0} originY={0} />
                </div>
              {/if}
            {/each}
            {#each model.parts.machines as m (m.name)}
              <div class="machine" data-machine={m.name}>
                <span class="mark" aria-hidden="true">{HOLDER_MARK.automaton}</span>
                <b>{m.name}</b>
                <span class="muted">{m.holding === null ? 'claims unread' : `holds ${m.holding} claim${m.holding === 1 ? '' : 's'}`} · health not read</span>
              </div>
            {/each}
            {#if model.parts.stations.length === 0 && model.parts.machines.length === 0}
              <p class="none">No sub-station and no machine.</p>
            {/if}
          </div>
        </section>

        <!-- station line (car B): the busiest owned protocol's line, each stop a
             station box with its admission queue, seats and bays (design 08d1530e). -->
        {#key model.name}
          <StationLine tree={tree.data} station={model.name} onMoving={(m) => (moving = m)} />
        {/key}

        <!-- WORKING: the claims inside the station, by part. -->
        <section class="sect" id="working" data-band="flow" aria-labelledby="h-working">
          <h2 id="h-working" class="eyebrow">Working <span class="sub">claimed at {model.name}, by the part holding it</span></h2>
          <div class="col" data-column="working">
            {#if model.working.state === 'loading'}
              <p role="status">reading the claims…</p>
            {:else if model.working.state === 'failed'}
              <p class="load-failed" data-working-failed>the claims read failed, so nothing claimed is drawn: {model.working.error}</p>
            {:else}
              {#each model.working.groups as g (g.part)}
                <div class="group" data-working-group={g.part}>
                  <p class="gname">{g.here ? `${g.part} itself` : g.part}</p>
                  {#each g.rows as r (r.key)}
                    {#snippet held()}<span class="k"><span class="mark" data-mark>{r.mark ?? '?'}</span> {r.holder}{#if r.since !== null} · since <LocalUtc at={r.since} />{/if}</span><span class="t">{r.what}</span>{/snippet}
                    <!-- A claim names its packet or it does not; only the first is a link. -->
                    {#if r.href !== null}
                      <a class="pk" href={safeLinkHref(r.href)} data-working-row={r.key}>{@render held()}</a>
                    {:else}
                      <div class="pk" data-working-row={r.key}>{@render held()}</div>
                    {/if}
                  {/each}
                  {#if g.more > 0}<p class="more" data-more="working">+{g.more} more</p>{/if}
                </div>
              {:else}
                <p class="none">nothing claimed here</p>
              {/each}
              {#if model.working.unplaced > 0}
                <p class="note" data-unplaced>{model.working.unplaced} open claim{model.working.unplaced === 1 ? '' : 's'} the record cannot place at any station</p>
              {/if}
            {/if}
          </div>
          <p class="legend"><span>● person</span><span>◆ robot</span><span>■ automaton</span></p>
        </section>
      </div>

      {#if ladder !== null && stuck !== null}
        <StationRail name={model.name} {ladder} bands={model.bands} caveat={model.poolsCaveat} {moving} {stuck} needs={needsList} {kinds} />
      {/if}
    </div>
  {/if}
</section>

<style>
  /* Enamel on the Transit palette (design a4df741a): light only, night-ink
     frames, 4px corners, comfortable rows. The band and rail follow E2 of
     design 08d1530e. */
  .station-page { background: var(--void); color: var(--fog); padding-bottom: var(--s5); }
  .crumbs { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s1) var(--s2); padding: var(--s2) 0;
    font-family: var(--font-mono); font-size: 12px; letter-spacing: 0.04em; }
  .crumb { color: var(--signal); text-decoration: none; }
  .crumb:hover { text-decoration: underline; }
  .crumb.current { color: var(--fog); font-weight: 600; }
  .crumb[data-troubled='true'] { color: var(--troubled-ink); }
  .sep { color: var(--static); }
  .climb { color: var(--on-troubled); background: var(--troubled); border-radius: 3px; padding: 1px 6px; font-size: 10.5px; }
  .grow { flex: 1; }
  .floor-link { color: var(--signal); font-size: 11px; text-decoration: none; }
  .num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }

  /* The answer band: the line's rail, the name, three linked counts, and
     the binding plate. */
  .head { margin-bottom: var(--s3); }
  .band { display: flex; align-items: stretch; flex-wrap: wrap; background: var(--band); color: var(--on-band);
    border-radius: 4px; overflow: hidden; min-height: 56px; }
  .band.adhoc { outline: 2px dashed var(--static); outline-offset: -2px; }
  .rail { width: 6px; flex: none; background: var(--line); }
  .rail.hollow { background: var(--band); box-shadow: inset 0 0 0 2px var(--line); }
  .rail.failed { background: var(--troubled); }
  .id { display: flex; flex-direction: column; justify-content: center; padding: 8px 16px 8px 12px; border-right: 1px solid var(--band-rule); }
  .name { margin: 0; font-size: 20px; font-weight: 800; line-height: 1.1; }
  .title { font-size: 12px; color: var(--on-band-dim); }
  .mode { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--on-band-dim); }
  /* The three counts share one row, their words wrapping inside it,
     before any of them drops to a row of its own. */
  .answers { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); flex: 1 1 360px; }
  .ans { display: flex; align-items: center; gap: 10px; padding: 8px 16px; color: var(--on-band); text-decoration: none; border-right: 1px solid var(--band-rule); min-width: 0; }
  .ans:hover { background: var(--band-raised); }
  .ans b { font-size: 22px; font-weight: 600; line-height: 1; }
  .ans .w { font-size: 12px; color: var(--on-band-dim); min-width: 0; overflow-wrap: anywhere; }
  .pl { display: inline-block; width: 12px; height: 12px; border-radius: 2px; flex: none; }
  .pl.clear { background: var(--clear); }
  .pl.troubled { background: var(--troubled); }
  .pl.you { background: var(--on-band); border-radius: 50%; }
  .pl.unread { outline: 1.5px dashed var(--band-rule); outline-offset: -1.5px; }
  .binding { margin-left: auto; display: flex; flex-direction: column; align-items: flex-start; justify-content: center; gap: 4px;
    padding: 8px 16px; font-size: 12px; max-width: 320px; }
  .binding .row { display: flex; align-items: center; gap: 8px; }
  .binding .lbl { color: var(--on-band-dim); }
  .binding .num { color: var(--on-band-dim); font-size: 11px; }
  .plate { font-family: var(--font-mono); font-size: 10.5px; font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase;
    padding: 4px 8px; border-radius: 3px; white-space: nowrap; }
  .plate.att { background: var(--busy); color: var(--on-busy); }
  .plate.quiet { color: var(--on-band-dim); outline: 1.5px solid var(--band-rule); outline-offset: -1.5px; }
  .plate.unread { color: var(--on-band-dim); outline: 1.5px dashed var(--band-rule); outline-offset: -1.5px; }
  .why { margin: var(--s1) 0 0; font-size: 12px; color: var(--troubled-ink); }

  /* Lines and parts on the left, constraints on the right (E2). */
  .layout { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: 0 var(--s4); }
  @media (max-width: 1100px) { .layout { grid-template-columns: minmax(0, 1fr); } }
  .main { min-width: 0; }

  .sect { margin-top: var(--s3); border-top: 1px solid var(--hairline); padding-top: var(--s3); }
  .sect:first-child { margin-top: 0; }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.24em; font-size: 11px; color: var(--fog); margin: 0 0 var(--s2); font-weight: 600; }
  .eyebrow .sub { text-transform: none; letter-spacing: 0; font-family: var(--font-body); font-weight: 400; color: var(--static); margin-left: var(--s2); }
  .muted, .sub { color: var(--static); font-size: 12px; }
  .note { font-size: 12px; color: var(--static); font-style: italic; margin: var(--s1) 0; }
  .none { font-size: 12px; color: var(--static); margin: var(--s1) 0; }

  .parts { display: flex; flex-wrap: wrap; gap: var(--s3); align-items: flex-start; }
  .part { position: relative; flex: none; }
  .machine { display: flex; flex-direction: column; gap: 2px; padding: 10px 12px; border: 2px solid var(--fog); border-radius: 4px; min-width: 220px; font-size: 12px; }
  .mark { font-family: var(--font-mono); }

  .col { border: 1px solid var(--hairline); border-radius: 4px; padding: var(--s2) var(--s3); background: var(--ink-raised); min-width: 0; }
  .group { margin-top: var(--s2); }
  .gname { margin: 0; font-family: var(--font-mono); font-size: 11px; color: var(--static); text-transform: uppercase; letter-spacing: 0.1em; }
  .pk { display: flex; flex-direction: column; padding: 6px 0; border-top: 1px solid var(--hairline); color: var(--fog); text-decoration: none; min-width: 0; }
  .pk:hover .t { text-decoration: underline; }
  .pk .k { font-family: var(--font-mono); font-size: 10.5px; color: var(--static); }
  .pk .t { font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .more { font-size: 12px; color: var(--static); margin: var(--s1) 0 0; }
  .legend { display: flex; gap: var(--s3); font-size: 11px; color: var(--static); margin-top: var(--s2); }
  .failed-read { position: relative; padding: var(--s3) var(--s3) var(--s3) 18px; border: 1px solid var(--troubled); border-radius: 4px; color: var(--troubled-ink); }
  .failed-read .rail { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 4px 0 0 4px; }
</style>
