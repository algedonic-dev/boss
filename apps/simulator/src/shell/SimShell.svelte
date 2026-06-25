<script lang="ts">
  // App shell for the Simulator UX. Mirrors the apps/web shell SHAPE —
  // a dark left sidebar under the perspective tab bar + a content
  // column — so the Simulator tab feels consistent with the System
  // Model and User Experiences tabs. The sidebar nav is the
  // simulator's own (Cockpit | Controls); it intentionally does NOT
  // mirror the User Experiences Work / Surfaces / Knowledge-Bases
  // grouping. New sim surfaces slot in as more NAV entries.
  import PerspectiveTabs from '@boss/web-kit/PerspectiveTabs.svelte';
  import { href, navigate, type Route } from '../router';

  let { route, children } = $props<{
    route: Route;
    children: () => unknown;
  }>();

  type NavItem = Readonly<{ label: string; rel: string; kind: Route['kind'] }>;
  const NAV: ReadonlyArray<NavItem> = [
    { label: 'Cockpit', rel: '/', kind: 'cockpit' },
    { label: 'Controls', rel: '/controls', kind: 'controls' },
  ];

  function go(e: MouseEvent, rel: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href(rel));
  }
</script>

<div class="sim-shell">
  <PerspectiveTabs active="simulator" brandName="Algedonic" brandSub="Ales" />
  <aside class="sim-sidebar">
    <nav class="sim-nav" aria-label="Simulator sections">
      <div class="sim-nav-group-label">Simulator</div>
      {#each NAV as item (item.kind)}
        <a
          class="sim-nav-item"
          class:active={route.kind === item.kind}
          href={href(item.rel)}
          aria-current={route.kind === item.kind ? 'page' : undefined}
          onclick={(e) => go(e, item.rel)}
        >{item.label}</a>
      {/each}
    </nav>
  </aside>

  <main class="sim-main">
    {@render children()}
  </main>
</div>

<style>
  .sim-shell {
    /* Neutral palette — kept local so the shell doesn't depend on
       apps/web's .theme-exec wrapper. */
    --bg: #fafaf9;
    --card: #ffffff;
    --border: #e7e5e4;
    --text: #1c1917;
    --text-dim: #78716c;

    display: grid;
    grid-template-columns: 200px 1fr;
    min-height: 100vh;
    /* Offset below the fixed 44px PerspectiveTabs bar. */
    padding-top: 44px;
    box-sizing: border-box;
    background: var(--bg);
    color: var(--text);
    font-family: var(--font-body);
  }
  /* Dark left rail, matching the apps/web shell sidebar. */
  .sim-sidebar {
    background: #1c1917;
    color: #e7e5e4;
    position: fixed;
    top: 44px;
    left: 0;
    bottom: 0;
    width: 200px;
    z-index: 20;
    overflow-y: auto;
  }
  .sim-nav {
    padding: 16px 8px 0;
  }
  .sim-nav-group-label {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: #a8a29e;
    padding: 4px 12px;
    margin-bottom: 4px;
  }
  .sim-nav-item {
    display: flex;
    align-items: center;
    padding: 6px 12px;
    border-radius: 6px;
    font-size: 13px;
    color: #d6d3d1;
    text-decoration: none;
    transition:
      background 0.1s,
      color 0.1s;
  }
  .sim-nav-item:hover {
    background: #292524;
    color: #fff;
  }
  .sim-nav-item.active {
    background: #44403c;
    color: #fff;
    font-weight: 500;
  }
  .sim-main {
    grid-column: 2;
    padding: 28px max(24px, calc((100vw - 200px - 1180px) / 2)) 64px;
  }
</style>
