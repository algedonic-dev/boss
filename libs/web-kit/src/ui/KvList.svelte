<script lang="ts">
  // <dl>-style key/value list. Rows come as children via the KvRow
  // sub-component so callers can inline conditional formatting.
  let { children } = $props<{ children: () => any }>();
</script>

<dl class="kv">
  {@render children()}
</dl>
