<script lang="ts" generics="K extends string">
  // Sortable <th> — pairs with createSortState (sort-state.svelte.ts).
  // Renders the column label + direction arrow, toggles on click or
  // Enter/Space, and announces the state via aria-sort.
  import type { SortState } from './sort-state.svelte';

  let { sort, key, num = false, children } = $props<{
    sort: SortState<K>;
    key: K;
    /// Right-align (the app's `.num` convention for numeric columns).
    num?: boolean;
    children: () => any;
  }>();

  function activate(): void {
    sort.toggle(key);
  }
</script>

<th
  class={num ? 'num' : undefined}
  aria-sort={sort.key === key
    ? sort.dir === 'asc'
      ? 'ascending'
      : 'descending'
    : undefined}
  tabindex="0"
  onclick={activate}
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      activate();
    }
  }}
>
  {@render children()}{sort.arrow(key)}
</th>

<style>
  th {
    cursor: pointer;
    user-select: none;
    white-space: nowrap;
  }
</style>
