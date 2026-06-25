<script lang="ts">
  // Tab bar — mirrors apps/web/src/ui/index.tsx Tabs.
  type Tab = { id: string; label: string };
  let {
    tabs,
    active = $bindable<string>(),
    onchange,
  } = $props<{
    tabs: ReadonlyArray<Tab>;
    active: string;
    onchange?: (id: string) => void;
  }>();

  function select(id: string) {
    active = id;
    onchange?.(id);
  }
</script>

<nav class="tabs" role="tablist">
  {#each tabs as t (t.id)}
    <button
      type="button"
      role="tab"
      aria-selected={active === t.id}
      class="tab {active === t.id ? 'tab-active' : ''}"
      onclick={() => select(t.id)}
    >
      {t.label}
    </button>
  {/each}
</nav>
