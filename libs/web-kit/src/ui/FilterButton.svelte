<script lang="ts">
  let { active, onclick, children } = $props<{
    active: boolean;
    onclick: () => void;
    children: () => any;
  }>();
</script>

<button
  type="button"
  class="filter-btn {active ? 'filter-btn-active' : ''}"
  {onclick}
>
  {@render children()}
</button>
