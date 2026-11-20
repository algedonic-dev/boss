<script lang="ts">
  // THE STATION PAGE — what opening a station shows (design c4854fd4 E2,
  // decided by David 2026-11-07 23:38Z; backlog 72d58e00; re-cut by design
  // 08d1530e car A, backlog 950ad972). The header ANSWERS: Working · Stuck
  // · Needs you as linked counts, and the binding constraint. The BODY is
  // the live floor (StationFloor.svelte, backlog dd0eb058, decided by
  // David on the prototype canvas 2026-11-19): the station's admission
  // machine, each part at its fixed place with its admission head as
  // tokens, the age-banded pile behind it and its working slots, the
  // routes, the Outbound record and the peers it names, moving on the live
  // events stream. The floor replaced the parts card grid, the right rail
  // (waits ladder, pool strips, moves) and the station line; Stuck and
  // Needs you, which the band counts, are listed under it. Every figure is
  // station-page.ts's, station-answer.ts's or station-floor.ts's, from the
  // real, current reads, and nothing here decides a number. Opening a
  // part opens the same view one level down — a view swap to its own
  // page, never a camera zoom (David 2026-09-20).
  //
  // NO STATION ASKED FOR IS THE ROOT (R3b of the transit-map retirement,
  // backlog 23a4b4d7): /it draws the company station's page, found by the
  // tree's shape — the one tree row with no parent — never by name, so
  // the live registry's `algedonic` and a fixture's `company` read alike.
  // Under another page's header (/it) its heading is an h2.
  //
  // EACH READ IS SAID WHEN IT FAILS. The tree is the page: a failed tree
  // read draws the decided red rail and no figure. A failed claims read
  // keeps what the tree holds, fills no working slot, and Working, Stuck
  // and the binding say why; a failed load read leaves the admission and
  // the queues unread; a failed human-required read leaves Needs you
  // unread; the floor says its own (the queues, the flow, the stream).
  import { navigate } from '../../router';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { parseStationLoad, type StationLoadRow } from '../marshalling/marshalling';
  import { fetchNeedsYou, type NeedsYou } from '../yard/human-needs';
  import { safeLinkHref } from '@boss/web-kit/links';
  import StationFloor from './StationFloor.svelte';
  import { resourcesHref } from './resources/resources';
  import { claimsPath, parseTree, rootIdOf, treeHref, type StationTree } from './station-tree';
  import { parsePageClaims, stationPage, type PageClaims, type PageReads } from './station-page';
  import {
    answerBand, bindingPlate, heldKinds, needsBand, needsYouList, needsYouUrl, stuckList, subtitleOf,
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
  /** When the reads were taken: the ages the band states. */
  let now = $state(Date.now());

  const loadPath = '/api/stations/load';
  const parseLoad = (raw: unknown) => new Map(parseStationLoad(raw).map((r) => [r.station, r] as const));

  /** The whole tree, from its root: the breadcrumb is the parent chain,
   *  and a peer on the floor is a station outside this one. The root is
   *  found by the tree's shape, never by a name. */
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
    void fetchRemote(loadPath, parseLoad).then((r) => { if (!gone) load = r; });
    // Its own subtree's claims, never every claim (backlog d05e29a7);
    // the root (no station asked) reads them all.
    void fetchRemote(claimsPath(asked), parsePageClaims).then((r) => { if (!gone) claims = r; });
    return () => { gone = true; };
  });

  /** The floor asks for the claims and the load again when the record
   *  moved. A failed re-read keeps the last answer drawn: the floor stands
   *  as the record last read, never blank. */
  function refresh(): void {
    const asked = station;
    void fetchRemote(claimsPath(asked), parsePageClaims).then((r) => { if (r.kind === 'ready' && station === asked) { claims = r; now = Date.now(); } });
    void fetchRemote(loadPath, parseLoad).then((r) => { if (r.kind === 'ready' && station === asked) load = r; });
  }

  /** The station drawn: the one asked for, or the root the tree read names. */
  const name = $derived(station ?? (tree.kind === 'ready' ? tree.data.top : null));

  /** The protocols this station's stations queue: what Needs you is
   *  scoped to (the root asks for all). */
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
  const model = $derived(tree.kind === 'ready' && name !== null ? stationPage(tree.data, name, reads) : null);
  /** The rail: the department's line, red when a read failed. */
  const line = $derived(model === null || model.card.rail.line === null ? 'var(--fog)' : `var(--map-line-${model.card.rail.line}, var(--fog))`);

  const answer = $derived(tree.kind === 'ready' && name !== null ? answerBand(tree.data, name, load, claims, now) : null);
  const binding = $derived(bindingPlate(claims));
  const stuck = $derived(tree.kind === 'ready' && name !== null ? stuckList(tree.data, name, load, claims, now) : null);
  const needsList = $derived<NeedsList>(kinds.kind === 'unread' || kinds.kind === 'none'
    ? { kind: 'unread', why: kinds.why, count: { kind: 'unread', why: kinds.why } }
    : needsYouList(needs, now));
  const subtitle = $derived(model === null ? null : subtitleOf(model.name, model.title));

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
      <!-- Work / Resources (backlog 87575d69): the same station, two views. -->
      <span class="view-toggle" data-view-toggle><span class="on">Work</span><a href={safeLinkHref(resourcesHref(model.name))} onclick={(e) => go(e, resourcesHref(model.name))} data-view="resources">Resources</a></span>
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
            <a class="ans" href="#floor" data-answer="working" data-answer-state={answer.working.kind}>{@render count('working', answer.working, 'clear')}</a>
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

    <!-- THE FLOOR: the body (backlog dd0eb058). Keyed by the station, so
         opening another one starts its own reads and its own stream. -->
    {#key model.name}
      <StationFloor tree={tree.data} station={model.name} {load} {claims} {refresh} />
    {/key}

    <!-- What the band counts, listed: Stuck and Needs you. -->
    <div class="lists">
      <section id="stuck" class="list-sect" aria-labelledby="h-stuck">
        <h2 id="h-stuck">Stuck <span class="muted">an idle seat with a queue, a claim at no station</span></h2>
        {#if stuck === null || stuck.kind === 'unread'}
          <p class="note" data-stuck-unread>not read: {stuck?.kind === 'unread' ? stuck.why : 'reading'}</p>
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

      <section id="needs-you" class="list-sect" aria-labelledby="h-needs">
        <h2 id="h-needs">Needs you <span class="muted">human-required steps</span></h2>
        {#if needsList.kind === 'unread'}
          <!-- Unread in quiet ink, not the failure rail: a server that does
               not answer the canonical human-required view cannot be read
               as "nothing needs you", and it is not this page failing. -->
          <p class="note" data-needs-unread>not read: {needsList.why}</p>
        {:else}
          <ul class="list">
            {#each needsList.lines as l (l.id)}
              <li data-need={l.id}><a href={safeLinkHref(l.href)}>{l.title}</a><span class="num">{l.age}</span><span class="s">{l.step}</span></li>
            {:else}
              <li class="none" data-no-needs>No step here waits on a person.</li>
            {/each}
            {#if needsList.more > 0}<li class="more" data-more="needs">+{needsList.more} more</li>{/if}
          </ul>
          {#if needsList.cut !== null}<p class="note" data-needs-cut>{needsList.cut}</p>{/if}
        {/if}
      </section>
    </div>
  {/if}
</section>

<style>
  /* Enamel on the Transit palette (design a4df741a): light only, night-ink
     frames, 4px corners, comfortable rows. The band follows E2 of design
     08d1530e; the floor is StationFloor.svelte's. */
  .station-page { background: var(--void); color: var(--fog); padding-bottom: var(--s5); min-width: 0; }
  .crumbs { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s1) var(--s2); padding: var(--s2) 0;
    font-family: var(--font-mono); font-size: 12px; letter-spacing: 0.04em; }
  .crumb { color: var(--signal); text-decoration: none; }
  .crumb:hover { text-decoration: underline; }
  .crumb.current { color: var(--fog); font-weight: 600; }
  .crumb[data-troubled='true'] { color: var(--troubled-ink); }
  .sep { color: var(--static); }
  .climb { color: var(--on-troubled); background: var(--troubled); border-radius: 3px; padding: 1px 6px; font-size: 10.5px; }
  .grow { flex: 1; }
  .view-toggle { display: inline-flex; margin-left: var(--s2); border: 1px solid var(--hairline); border-radius: 4px; overflow: hidden; }
  .view-toggle > * { padding: 2px 10px; color: var(--fog); text-decoration: none; }
  .view-toggle .on { background: var(--fog); color: var(--void); }
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

  /* Stuck and Needs you under the floor, side by side where they fit. */
  .lists { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 0 var(--s4); margin-top: var(--s3); }
  .list-sect { border-top: 2px solid var(--fog); padding-top: 7px; margin-bottom: var(--s3); min-width: 0; }
  .list-sect h2 { font-size: 14px; font-weight: 800; margin: 0 0 6px; display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
  .muted { font-weight: 400; font-size: 12px; color: var(--static); }
  .list-sect a { color: var(--signal); text-decoration: none; }
  .list-sect a:hover { text-decoration: underline; }
  .note { font-size: 11px; color: var(--static); margin: 4px 0 0; }
  .none { font-size: 12px; color: var(--static); margin: var(--s1) 0; }
  .more { font-size: 12px; color: var(--static); margin: 4px 0 0; }
  .list { list-style: none; margin: 0; padding: 0; }
  .list li { display: grid; grid-template-columns: 1fr auto; gap: 0 8px; padding: 4px 0; border-bottom: 1px solid var(--hairline); font-size: 12px; }
  .list li a { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .list li .s { grid-column: 1 / 3; font-size: 10.5px; color: var(--static); }
  .failed-read { position: relative; padding: var(--s3) var(--s3) var(--s3) 18px; border: 1px solid var(--troubled); border-radius: 4px; color: var(--troubled-ink); }
  .failed-read .rail { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 4px 0 0 4px; }
</style>
