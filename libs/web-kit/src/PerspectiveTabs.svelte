<script lang="ts">
  // Top-level perspective switcher across the three BOSS surfaces, each
  // (increasingly) its own app served by its own piece:
  //   Simulator        — run/observe the sim          (boss-simulator, /simulator)
  //   System Model      — define/understand the model  (the model-definition surfaces, /it)
  //   User Experiences  — the interfaces actors work in (the operator surfaces, /)
  //
  // Rendered fixed across the very top of every app's shell. It is the
  // single home for the top chrome: the tenant wordmark (left), the
  // perspective tabs (center), and the shared right-hand controls —
  // the system-time indicator and the sign-in/out control. Switching
  // perspective is a full navigation (the perspectives are served
  // distinctly), so the tabs are plain anchors, not client-side routes.
  // 44px tall — each shell offsets its chrome below it.
  import SystemTime from './SystemTime.svelte';
  import SignInControl from './SignInControl.svelte';

  type Perspective = 'simulator' | 'model' | 'user';
  let {
    active,
    brandName = 'BOSS',
    brandSub = '',
  } = $props<{
    active: Perspective;
    brandName?: string;
    brandSub?: string;
  }>();

  const TABS: ReadonlyArray<{ id: Perspective; label: string; href: string }> = [
    { id: 'simulator', label: 'Simulator', href: '/simulator' },
    { id: 'model', label: 'System Model', href: '/it' },
    { id: 'user', label: 'User Experiences', href: '/' },
  ];
</script>

<nav class="perspective-tabs" aria-label="Perspective">
  <span class="perspective-brand">
    <span class="perspective-brand-name">{brandName}</span>
    {#if brandSub}<span class="perspective-brand-sub">{brandSub}</span>{/if}
  </span>
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
  <div class="perspective-right">
    <SystemTime />
    <SignInControl />
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
    gap: 20px;
    background: #0c0a09;
    border-bottom: 1px solid #292524;
    padding: 0 16px;
  }
  .perspective-brand {
    display: flex;
    align-items: baseline;
    gap: 5px;
    flex: 0 0 auto;
  }
  .perspective-brand-name {
    font-family: var(--font-display, inherit);
    font-size: 16px;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: #fafaf9;
  }
  .perspective-brand-sub {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--brew-amber, #d99b3a);
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
    /* Reserve the active underline on every tab so selecting one
       doesn't shift the row. */
    border-bottom: 3px solid transparent;
    transition:
      color 0.1s,
      background 0.1s,
      border-color 0.1s;
  }
  .perspective-tab:hover {
    color: #e7e5e4;
    background: rgba(255, 255, 255, 0.04);
  }
  /* Selected perspective: amber-tinted fill + bright bold label + a
     thick amber underline — distinctly "you are here" against the
     muted inactive tabs. */
  .perspective-tab.active {
    color: #fff;
    font-weight: 700;
    background: rgba(217, 155, 58, 0.18);
    border-bottom-color: var(--brew-amber, #d99b3a);
  }
  .perspective-tab.active:hover {
    background: rgba(217, 155, 58, 0.24);
  }
  .perspective-right {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 14px;
    flex: 0 0 auto;
  }
</style>
