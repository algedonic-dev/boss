<script lang="ts">
  // Simple app shell for the Simulator UX. Deliberately NOT the
  // apps/web AppShell (too coupled to that app's sidebar/session
  // model). A topbar with the brand, the shared <SystemTime> clock
  // indicator, and a two-item nav (Cockpit | Controls). The page
  // content renders in the snippet below.
  //
  // Neutral palette variables (--bg/--border/--text/...) are scoped to
  // .sim-shell here rather than relying on apps/web's `.theme-exec`
  // wrapper — keeps the shell self-contained. The brewery accent vars
  // (--brew-*) come from styles.css :root.
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
    e.preventDefault();
    navigate(href(rel));
  }
</script>

<div class="sim-shell">
  <PerspectiveTabs active="simulator" brandName="Algedonic" brandSub="Ales" />
  <header class="sim-topbar">
    <div class="sim-brand">
      <span class="sim-brand-name">BOSS</span>
      <span class="sim-brand-sub">Simulator</span>
    </div>
    <nav class="sim-nav" aria-label="Simulator sections">
      {#each NAV as item (item.kind)}
        <a
          class="sim-nav-link"
          class:active={route.kind === item.kind}
          href={href(item.rel)}
          aria-current={route.kind === item.kind ? 'page' : undefined}
          onclick={(e) => go(e, item.rel)}
        >{item.label}</a>
      {/each}
    </nav>
  </header>

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

    min-height: 100vh;
    /* Offset below the fixed 44px PerspectiveTabs bar. */
    padding-top: 44px;
    box-sizing: border-box;
    background: var(--bg);
    color: var(--text);
    font-family: var(--font-body);
  }
  .sim-topbar {
    display: flex;
    align-items: center;
    gap: 24px;
    padding: 12px max(20px, calc((100vw - 1280px) / 2));
    background: var(--card);
    border-bottom: 2px solid var(--brew-amber);
    position: sticky;
    top: 44px;
    z-index: 10;
  }
  .sim-brand {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .sim-brand-name {
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: var(--brew-malt-dark);
  }
  .sim-brand-sub {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--brew-amber);
  }
  .sim-nav {
    display: flex;
    gap: 4px;
    flex: 1 1 auto;
  }
  .sim-nav-link {
    padding: 6px 12px;
    border-radius: 6px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-dim);
    text-decoration: none;
    transition: background 80ms, color 80ms;
  }
  .sim-nav-link:hover {
    background: #f5f5f4;
    color: var(--text);
  }
  .sim-nav-link.active {
    background: var(--brew-amber-bg);
    color: var(--brew-malt);
    font-weight: 600;
  }
  .sim-main {
    padding: 28px max(20px, calc((100vw - 1280px) / 2)) 64px;
  }
</style>
