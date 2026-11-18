<script lang="ts">
  // THE DEPARTMENT MAP — the /it landing (design e765b3fc). Since R3 of the
  // transit-map retirement (backlog 23a4b4d7; conductor orders
  // 20261108T1500Z and 20261108T1523Z, David ~15:25Z: "we seem to be
  // preserving a lot of information that isn't readily being consumed by
  // me but is still being shown") it draws only what David reads and
  // uses, all of it real, current activity (David, 2026-11-08 00:50Z):
  //
  //   - NEEDS YOU: the human-required IT jobs, approvals and ops requests
  //     among them (NeedsYou.svelte, human-needs.ts);
  //   - THE ALARMS BOARD: every station neither clear nor full, with the
  //     server's why, from GET /api/yard/regions (alarms.ts) — each a
  //     door to that station's boards below;
  //   - THE COMPANY STATION'S PAGE: the root of the station tree (on the
  //     live registry, `algedonic`) — its constraints, its departments as
  //     station cards, the motion line and In / Working / Out
  //     (StationPage.svelte; R3b, backlog 23a4b4d7). The whole floor at
  //     every depth stays at /it/tree;
  //   - A STATION'S BOARDS, when `/it?at=<name>` selects one: the boards
  //     the region pages retired into (design e765b3fc, car N3) — the
  //     floor deck, the queue boards, the release and cancel controls.
  //
  // Gone with the transit map: the map and its routes, the HUD frame's
  // other rows (outranks, next up, machines, binding plate), the flow
  // counts and their 'as of' cut, the border rates, the panel's rate and
  // trend cells, the phone strip and the plant strip, and the estate
  // band, delivery belt, firings and signals tape under the floor.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import type { Remote } from '../../data/remote';
  import { fetchRegions, regionHref, REGION_NAMES, type Regions } from './regions';
  import { alarmLabel, alarmsOf } from './alarms';
  import { fetchNeedsYou, type NeedsYou as NeedsYouRead } from './human-needs';
  import { boardsOf } from './panel';
  import { hasInterior } from './region-contents';
  import { hasPlatforms } from './world-interior';
  import NeedsYou from './NeedsYou.svelte';
  import StationPage from '../tree/StationPage.svelte';
  import StationBoundary from './StationBoundary.svelte';
  import FloorDeck from './FloorDeck.svelte';
  import YardStatusPanel from './YardStatusPanel.svelte';
  import ConductorFeed from '../monitoring/ConductorFeed.svelte';
  import SensorsPanel from '../sensors/SensorsPanel.svelte';
  import ReceivingYardPage from '../receiving/ReceivingYardPage.svelte';
  import MarshallingYardPage from '../marshalling/MarshallingYardPage.svelte';
  import FeedbackTriagePage from '../feedback/FeedbackTriagePage.svelte';
  import BacklogBoardPage from '../backlog/BacklogBoardPage.svelte';
  import { mapRead, startMapPolling } from './map-polling';

  type Props = Readonly<{
    /** The station whose boards open below — `/it?at=<name>`. Absent,
     *  none opens. */
    at?: string;
  }>;
  let { at = undefined }: Props = $props();

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let needs = $state<Remote<NeedsYouRead>>({ kind: 'loading' });
  let successfulReads = $state(0);

  onMount(() => {
    // One read in flight per endpoint (452e507c, 909adf3b); a hidden tab
    // adds no background reads. map-polling.ts says why.
    return startMapPolling([
      mapRead(fetchRegions, (r) => {
        regions = r;
        if (r.kind === 'ready') successfulReads += 1;
      }),
      mapRead(fetchNeedsYou, (n) => { needs = n; }),
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
  <a class="company-back" href="/it/tree" onclick={(e) => go(e, '/it/tree')}>Company station tree</a>
  <PageHeader
    eyebrow="IT"
    title="Department Map"
    subtitle="What needs you, the alarms, and the company station as it stands now. Open an alarm for that station's boards below."
  />

  <NeedsYou {needs} />

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
        <a class="panel-close" data-close href="/it" aria-label="close the {at} selection" onclick={(e) => go(e, '/it')}>close</a>
      </header>
      {#if station === null}
        <p class="panel-none">No station here is named “{at}”.</p>
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

  <StationPage station={null} embedded />
</div>

<!-- WHAT STANDS IN A STATION — its floor's deck or queue board, and the
     boards of the pages that retired into it (design e765b3fc, cars N2
     and N3). Keyed on the station, so a move from one to another opens
     the new station's boards. -->
{#snippet contents(region: string)}
  {#key region}
    {#if hasInterior(region)}
      <FloorDeck focus={region} />
    {:else if hasPlatforms(region)}
      {#if region === 'receiving'}
        <ReceivingYardPage embedded />
      {:else if region === 'marshalling'}
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
