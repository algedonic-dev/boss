<script lang="ts">
  // The chrome bar — the one thing every app shares.
  //
  // Tenant wordmark (left), the app tabs (centre), and the shared
  // right-hand controls: global search, feedback, system time and the
  // sign-in/out control.
  // Everything below it belongs to whichever app is active; this bar
  // is the only fixed furniture. 44px tall — each app's shell offsets
  // its own chrome below it.
  //
  // The tab list is APPS from @boss/web-kit/nav, so the bar and the
  // surface-to-app mapping cannot disagree about which apps exist.
  // Tabs are plain anchors: Simulator is served by a different piece
  // (boss-simulator) so switching to it is a real navigation, and for
  // the same-SPA apps the router picks the change up on popstate.
  import SystemTime from './SystemTime.svelte';
  import SignInControl from './SignInControl.svelte';
  import GlobalSearch from './GlobalSearch.svelte';
  import FeedbackControl from './FeedbackControl.svelte';
  import { APPS, type AppId } from './nav';
  import { manifest } from './session/manifest.svelte';

  let {
    active,
    brandName: brandNameProp,
    brandSub: brandSubProp,
    searchAppKinds = [] as ReadonlyArray<string>,
  } = $props<{
    active: AppId;
    /// Overrides the tenant's own name. Left unset everywhere in the
    /// shipped apps — the brand comes from the manifest, because
    /// hardcoding it is how three render sites came to disagree and
    /// how a second tenant ended up showing a brewery's name.
    brandName?: string;
    brandSub?: string;
    /// Subject kinds of the active app, passed to global search as a
    /// ranking hint. web-kit deliberately does not know the mapping —
    /// the host owns which surfaces belong to which app.
    searchAppKinds?: ReadonlyArray<string>;
  }>();

  /// The tenant's own name, split on its last word so "Algedonic Ales"
  /// still renders as a wordmark plus a lighter suffix — the shape the
  /// hardcoded props produced, now derived rather than repeated.
  ///
  /// Falls back to "BOSS" while the manifest loads and for a
  /// deployment that has not named itself. A prop still wins, for a
  /// host that genuinely needs to override.
  let brand = $derived.by(() => {
    const name =
      brandNameProp !== undefined
        ? undefined
        : manifest.value.kind === 'ready'
          ? manifest.value.displayName
          : undefined;
    if (brandNameProp !== undefined) {
      return { name: brandNameProp, sub: brandSubProp ?? '' };
    }
    if (!name) return { name: 'BOSS', sub: '' };
    const words = name.trim().split(/\s+/);
    return words.length > 1
      ? { name: words.slice(0, -1).join(' '), sub: words[words.length - 1]! }
      : { name, sub: '' };
  });
  let brandName = $derived(brand.name);
  let brandSub = $derived(brand.sub);
</script>

<nav class="perspective-tabs" aria-label="Perspective">
  <span class="perspective-brand">
    <span class="perspective-brand-name">{brandName}</span>
    {#if brandSub}<span class="perspective-brand-sub">{brandSub}</span>{/if}
  </span>
  <div class="perspective-tablist">
    {#each APPS as t (t.id)}
      <a
        class="perspective-tab"
        class:active={active === t.id}
        href={t.href}
        aria-current={active === t.id ? 'page' : undefined}
      >{t.label}</a>
    {/each}
  </div>
  <div class="perspective-right">
    <GlobalSearch appKinds={searchAppKinds} />
    <FeedbackControl />
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
