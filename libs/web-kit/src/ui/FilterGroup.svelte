<script lang="ts">
  let { label, children } = $props<{ label: string; children: () => any }>();
</script>

<div class="filter-group">
  <div class="filter-label">{label}</div>
  {@render children()}
</div>
