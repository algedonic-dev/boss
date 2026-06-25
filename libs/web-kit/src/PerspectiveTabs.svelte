<script lang="ts">
  // Top-level perspective switcher across the three BOSS surfaces, each
  // (increasingly) its own app served by its own piece:
  //   Simulator        — run/observe the sim          (boss-simulator, /simulator)
  //   System Model      — define/understand the model  (the model-definition surfaces, /it)
  //   User Experiences  — the interfaces actors work in (the operator surfaces, /)
  //
  // Rendered fixed across the very top of every app's shell. Switching
  // perspective is a full navigation (the perspectives are served
  // distinctly), so these are plain anchors, not client-side routes.
  // 44px tall — each shell offsets its chrome below it.
  type Perspective = 'simulator' | 'model' | 'user';
  let { active } = $props<{ active: Perspective }>();

  const TABS: ReadonlyArray<{ id: Perspective; label: string; href: string }> = [
    { id: 'simulator', label: 'Simulator', href: '/simulator' },
    { id: 'model', label: 'System Model', href: '/it' },
    { id: 'user', label: 'User Experiences', href: '/' },
  ];
</script>

<nav class="perspective-tabs" aria-label="Perspective">
  <span class="perspective-brand">BOSS</span>
  <div class="perspective-tablist">
    {#each TABS as t (t.id)}
      <a
        class="perspective-tab"
        class:active={active === t.id}
        href={t.href}
        aria-current={active === t.id ? 'page' : undefined}
      >{t.label}</a>
    {/each}
  </div>
</nav>

<style>
  .perspective-tabs {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 44px;
    z-index: 60;
    display: flex;
    align-items: stretch;
    gap: 18px;
    background: #0c0a09;
    border-bottom: 1px solid #292524;
    padding: 0 16px;
  }
  .perspective-brand {
    display: flex;
    align-items: center;
    font-family: var(--font-display, inherit);
    font-size: 15px;
    font-weight: 700;
    letter-spacing: 0.02em;
    color: #fafaf9;
  }
  .perspective-tablist {
    display: flex;
    align-items: stretch;
  }
  .perspective-tab {
    display: flex;
    align-items: center;
    padding: 0 18px;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.01em;
    color: #a8a29e;
    text-decoration: none;
    border-bottom: 2px solid transparent;
    transition:
      color 0.1s,
      border-color 0.1s;
  }
  .perspective-tab:hover {
    color: #e7e5e4;
  }
  .perspective-tab.active {
    color: #fff;
    border-bottom-color: var(--brew-amber, #d99b3a);
  }
</style>
