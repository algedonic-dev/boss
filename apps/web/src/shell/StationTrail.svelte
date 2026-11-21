<script lang="ts">
  // THE TOP BAR'S NAVIGATION: Home, then the path through the station tree
  // from the company root to the station being viewed (backlog 474e2c1c,
  // David 2026-11-19). It took the place of one tab per row of the
  // departments registry, which drew departments the company no longer
  // has. What it draws is shell/station-trail.ts's; this file owns the
  // strokes, the phone width and the one expander.
  import { PHONE_QUERY } from '@boss/web-kit/ui/phone';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { href, navigate } from '../router';
  import { ROUTE_CATALOG } from './nav-catalog';
  import { trailView, type Trail, type TrailStop } from './station-trail';

  let { trail, home = false, simulator = false } = $props<{
    trail: Trail;
    /// The page is the person's own (Home). Said apart from the trail
    /// because being Home needs no tree: an unread tree never takes away
    /// where you are.
    home?: boolean;
    /// The Simulator drives the playground's model and is no station; it
    /// keeps its one link for a tenant whose manifest lists `sim`
    /// (ce68f137), served by its own piece, so a plain navigation.
    simulator?: boolean;
  }>();

  // Home is the person's own station, /me: the catalog's path for My Day,
  // which needs no tree, so Home is a link even while the tree is unread.
  const HOME = ROUTE_CATALOG.me.path ?? '/me';

  // A phone folds every station between the root and the current one; a
  // wide screen folds only a deep path (station-trail.ts `trailView`).
  let narrow = $state(window.matchMedia?.(PHONE_QUERY).matches === true);
  $effect(() => {
    const mq = window.matchMedia?.(PHONE_QUERY);
    if (mq === undefined) return;
    const on = () => (narrow = mq.matches);
    on();
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  });

  let view = $derived(trail.kind === 'ready' ? trailView(trail.stops, { narrow }) : null);
  let open = $state(false);

  // A new trail closes the expander: it listed the last page's ancestors.
  $effect(() => {
    void trail;
    open = false;
  });

  // Escape closes it. A plain listener rather than a svelte:window tag,
  // which the bundler cannot resolve (no-svelte-window.test.ts).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') open = false;
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });

  function go(e: MouseEvent, to: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    open = false;
    navigate(to);
  }
</script>

{#snippet stop(s: TrailStop)}
  <span class="trail-sep" aria-hidden="true">/</span>
  {#if s.current}
    <span class="trail-stop trail-current" aria-current="page" title={s.title} data-trail-stop={s.name}>{s.label}</span>
  {:else}
    <a class="trail-stop" href={safeLinkHref(href(s.href))} title={s.title} onclick={(e) => go(e, s.href)} data-trail-stop={s.name}>{s.label}</a>
  {/if}
{/snippet}

<div class="trail" data-trail data-trail-state={trail.kind}>
  <a
    class="trail-home"
    class:trail-home-on={home}
    href={safeLinkHref(href(HOME))}
    aria-current={home ? 'page' : undefined}
    onclick={(e) => go(e, HOME)}
    data-trail-home>Home</a>
  {#if trail.kind === 'loading'}
    <span class="trail-note" role="status" data-trail-loading>Reading the station tree…</span>
  {:else if trail.kind === 'failed'}
    <!-- Said, never an empty bar: a bar with only Home reads as a company
         with no stations (builder rule 21.2). -->
    <span class="trail-note trail-failed" role="alert" data-trail-failed>Couldn't read the station tree — {trail.why}</span>
  {:else if view !== null}
    <nav class="trail-path" aria-label="Where this station stands">
      {#each view.head as s (s.id)}{@render stop(s)}{/each}
      {#if view.hidden.length > 0}
        <span class="trail-sep" aria-hidden="true">/</span>
        <span class="trail-fold">
          <button
            type="button"
            class="trail-stop trail-expander"
            aria-expanded={open}
            aria-haspopup="true"
            aria-label="{view.hidden.length} station{view.hidden.length === 1 ? '' : 's'} between"
            onclick={() => (open = !open)}
            data-trail-expander>…</button>
          {#if open}
            <button type="button" class="trail-scrim" aria-label="Close menu" onclick={() => (open = false)}></button>
            <span class="trail-menu" data-trail-hidden>
              {#each view.hidden as s (s.id)}
                <a class="trail-menu-item" href={safeLinkHref(href(s.href))} title={s.title} onclick={(e) => go(e, s.href)} data-trail-stop={s.name}>{s.label}</a>
              {/each}
            </span>
          {/if}
        </span>
      {/if}
      {#each view.tail as s (s.id)}{@render stop(s)}{/each}
    </nav>
  {/if}
  {#if simulator}
    <a class="trail-stop trail-app" href="/simulator" data-trail-simulator>Simulator</a>
  {/if}
</div>

<style>
  /* Words on the enamel band (PerspectiveTabs.svelte): every colour here
     is an on-band token, and the page you are on is the band's notch cut
     out to the page's own ground, the way the tab it replaced drew it. */
  /* Never shrinks below its words: on a phone the band scrolls sideways
     instead (PerspectiveTabs.svelte), so search never sits over a stop. */
  .trail {
    display: flex;
    align-items: stretch;
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .trail-home,
  .trail-stop {
    display: flex;
    align-items: center;
    margin-top: 8px;
    padding: 2px 12px 0;
    border-radius: var(--radius) var(--radius) 0 0;
    font-family: var(--font-mono);
    font-size: 12px;
    font-weight: 400;
    text-transform: uppercase;
    letter-spacing: var(--ls-label);
    color: var(--on-band-dim);
    text-decoration: none;
    background: none;
    border: none;
    cursor: pointer;
  }
  .trail-home:hover,
  .trail-stop:hover {
    color: var(--on-band);
    background: var(--band-raised);
  }
  .trail-home-on,
  .trail-home-on:hover,
  .trail-current,
  .trail-current:hover {
    color: var(--fog);
    font-weight: 600;
    background: var(--void);
    cursor: default;
  }
  .trail-path {
    display: flex;
    align-items: stretch;
    margin-left: 6px;
  }
  .trail-sep {
    display: flex;
    align-items: center;
    margin-top: 8px;
    color: var(--on-band-dim);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .trail-app {
    margin-left: 18px;
  }
  .trail-note {
    display: flex;
    align-items: center;
    margin-left: 12px;
    font-size: 12px;
    color: var(--on-band-dim);
  }
  .trail-failed {
    color: var(--on-band);
  }
  .trail-fold {
    position: relative;
    display: flex;
    align-items: stretch;
  }
  .trail-scrim {
    position: fixed;
    inset: 0;
    z-index: 1;
    border: none;
    padding: 0;
    background: transparent;
    cursor: default;
  }
  .trail-menu {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 2;
    min-width: 180px;
    padding: 6px;
    display: flex;
    flex-direction: column;
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-top: 0;
    border-radius: 0 0 var(--radius) var(--radius);
  }
  .trail-menu-item {
    padding: 7px 10px;
    border-radius: 4px;
    font-size: 13px;
    color: var(--static);
    text-decoration: none;
  }
  .trail-menu-item:hover {
    background: var(--wash);
    color: var(--fog);
  }
  /* On a phone the band scrolls sideways inside itself, which clips what
     hangs out of it, so the menu is pinned under the band instead. */
  @media (max-width: 720px) {
    .trail-menu { position: fixed; top: 44px; left: 8px; right: 8px; }
  }
</style>
