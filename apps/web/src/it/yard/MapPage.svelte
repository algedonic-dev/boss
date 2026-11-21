<script lang="ts">
  // THE ALARMS BOARD — what stays of the Department Map (design
  // e765b3fc) after step 1 of design 01fa5bdf (David, 2026-11-17: "IT
  // disappears, the old map is pulled out"; when unsure, remove). It
  // keeps the two things no other page offers yet:
  //
  //   - THE ALARMS BOARD: every station neither clear nor full, with the
  //     server's why, from GET /api/yard/regions (alarms.ts);
  //   - THE RELEASE AND CANCEL CONTROLS, under `/alarms?at=<name>`: the
  //     track's deck, where a troubled train is cancelled, and the yard
  //     status board's dock and garage lanes, where a held car is
  //     released (panel.ts names which station carries which).
  //
  // The station page takes both in that design's step 4, and this page
  // goes then; the regions read goes in its step 5, once the alarms read
  // station rows. Gone in step 1: the needs-you board (the person's
  // queue moves to /me), the embedded company station page (it stands at
  // /tree, the root's, since backlog 92bfcb04), and every other station's boards — the gates', arrivals'
  // and shed's decks, the receiving and marshalling queue boards, the
  // sensors panel, the conductor's feed and the feedback and backlog
  // boards.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import type { Remote } from '../../data/remote';
  import { BOARD_HREF, fetchRegions, regionHref, REGION_NAMES, type Regions } from './regions';
  import { treeHref } from '../tree/station-tree';
  import { alarmLabel, alarmsOf } from './alarms';
  import { boardsOf } from './panel';
  import StationBoundary from './StationBoundary.svelte';
  import FloorDeck from './FloorDeck.svelte';
  import YardStatusPanel from './YardStatusPanel.svelte';
  import { mapRead, startMapPolling } from './map-polling';

  type Props = Readonly<{
    /** The station whose boards open below — `/alarms?at=<name>`. Absent,
     *  none opens. */
    at?: string;
  }>;
  let { at = undefined }: Props = $props();

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let successfulReads = $state(0);

  onMount(() => {
    // One read in flight; a hidden tab adds no background reads.
    // map-polling.ts says why.
    return startMapPolling([
      mapRead(fetchRegions, (r) => {
        regions = r;
        if (r.kind === 'ready') successfulReads += 1;
      }),
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

  const alarms = $derived(regions.kind === 'ready' ? alarmsOf(regions.data) : null);
  /** A selection names a declared station or nothing: a typo is said. */
  const station = $derived(at === undefined ? null : (REGION_NAMES as ReadonlyArray<string>).includes(at) ? at : null);
  const boards = $derived(station === null ? [] : boardsOf(station));

  /** The station as a heading names it: `shop-floor` is "Shop floor". */
  const titleOf = (name: string): string => alarmLabel(name).replace(/^./, (c) => c.toUpperCase());

  let panel = $state<HTMLElement | null>(null);
  $effect(() => {
    if (at !== undefined && panel !== null) panel.scrollIntoView({ block: 'nearest' });
  });

  function go(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

<div class="theme-exec yard-root">
  <a class="company-back" href={safeLinkHref(treeHref(null))} onclick={(e) => go(e, treeHref(null))}>Company station tree</a>
  <PageHeader
    eyebrow="IT"
    title="Alarms"
    subtitle="Every station neither clear nor full. The track, the dock and the garage open the release and cancel controls below."
  />

  <!-- THE ALARMS BOARD (R2.5a, c1996aae; moved off the tree floor in R3):
       each is a door to its station's boards on this page. -->
  <aside class="alarms" data-alarms aria-label="alarms">
    <h2 class="eyebrow">Alarms</h2>
    {#if regions.kind === 'loading'}
      <p class="none" role="status">Reading the stations' states…</p>
    {:else if regions.kind === 'failed'}
      <p class="load-failed" data-alarms-failed>the regions read failed, so no alarm can be read: {regions.error}</p>
    {:else if alarms !== null}
      {#each alarms as a (a.name)}
        <a class="alarm" href={safeLinkHref(regionHref(a.name))} onclick={(e) => go(e, regionHref(a.name))}
          data-alarm={a.name} data-state={a.state} title={a.why}>
          <span class="alarm-head"><span class="alarm-name">{a.label}</span><span class="st {a.state}">{a.state}</span></span>
          <span class="why">{a.why}</span>
        </a>
      {:else}
        <p class="none" data-alarms-clear>No alarms: every station is clear or full.</p>
      {/each}
    {/if}
  </aside>

  {#if at !== undefined}
    <section class="map-panel" bind:this={panel} data-map-panel data-selection={at} tabindex="-1" aria-label="the {at} selection">
      <header class="panel-head">
        <span class="panel-kind">Station</span>
        <h2 class="panel-title">{titleOf(at)}</h2>
        <a class="panel-close" data-close href={safeLinkHref(BOARD_HREF)} aria-label="close the {at} selection" onclick={(e) => go(e, BOARD_HREF)}>close</a>
      </header>
      {#if station === null}
        <p class="panel-none">No station here is named “{at}”.</p>
      {:else if boards.length === 0}
        <p class="none" data-no-boards>This station's boards went with the map; its alarm above says what it reads.</p>
      {:else if regions.kind === 'loading'}
        <p class="none" role="status">Reading the stations' states…</p>
      {:else}
        <div class="panel-contents" data-contents={station}>
          <StationBoundary name={station} generation={successfulReads}>
            {@render contents(station)}
          </StationBoundary>
        </div>
      {/if}
    </section>
  {/if}
</div>

<!-- WHAT STANDS IN A STATION — the boards panel.ts names for it. Keyed
     on the station, so a move from one to another opens the new
     station's boards. -->
{#snippet contents(region: string)}
  {#key region}
    {#each boardsOf(region) as board (board)}
      <div class="panel-board" data-station-board={board}>
        {#if board === 'deck'}
          <FloorDeck focus={region} />
        {:else}
          <YardStatusPanel part={region === 'track' ? 'track' : region === 'dock' ? 'dock' : 'garage'} />
        {/if}
      </div>
    {/each}
  {/key}
{/snippet}

<style>
  .company-back { display: inline-block; margin-bottom: var(--s2); color: var(--map-ink); font-size: 13px; }
  .yard-root { padding: 0 32px 32px; }
  /* On a phone the shell's own 16px gutter is the page's (styles.css). */
  @media (max-width: 720px) { .yard-root { padding: 0 0 24px; } }
  .eyebrow { font-family: var(--font-mono); text-transform: uppercase; letter-spacing: 0.3em; font-size: 10px; color: var(--map-muted); margin: 0; font-weight: 400; }
  .alarms { background: var(--map-surface); border: 1px solid var(--map-rule); border-top: 3px solid var(--map-ink); border-radius: var(--radius); padding: var(--s2) var(--s3); margin: var(--s2) 0; color: var(--map-ink); }
  .alarm { display: block; border-top: 1px solid var(--map-rule); padding: var(--s2) 0; color: var(--map-ink); text-decoration: none; }
  .alarm:first-of-type { border-top: 0; }
  .alarm:hover .alarm-name { text-decoration: underline; }
  .alarm-head { display: flex; justify-content: space-between; gap: var(--s2); font-size: 13px; font-weight: 600; }
  .alarm .why { display: block; font-size: 12px; color: var(--map-muted); }
  .st.troubled { color: var(--map-bad-ink); }
  .st.attention { color: var(--map-warn-ink); }
  .none { font-size: 12px; color: var(--map-muted); margin: var(--s1) 0; }
  /* The selection's panel — a plate under the board: a hairline frame
     and a mono uppercase head. */
  .map-panel { margin: var(--s3) 0; border: 1px solid var(--map-rule-strong);
    border-top-width: 3px; background: var(--map-surface); padding: var(--s2) var(--s3) var(--s3);
    color: var(--map-ink); }
  .panel-head { display: flex; align-items: center; gap: var(--s2); font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; }
  .panel-kind { color: var(--map-muted); }
  .panel-title { margin: 0; font: inherit; font-weight: 600; color: var(--map-ink); }
  .panel-close { margin-left: auto; color: var(--map-muted); border: 1px solid var(--map-rule);
    padding: 2px 8px; text-decoration: none; }
  .panel-close:hover, .panel-close:focus-visible { color: var(--map-ink); border-color: var(--map-ink); }
  .panel-contents { margin-top: var(--s3); border-top: 1px solid var(--map-rule); }
  .panel-board { margin-top: var(--s3); }
  .panel-none { margin: var(--s2) 0 0; font-size: 13px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
</style>
