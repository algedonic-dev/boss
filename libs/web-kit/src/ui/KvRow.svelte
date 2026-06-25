<script lang="ts">
  let { label, children } = $props<{ label: string; children: () => any }>();
</script>

<dt>{label}</dt>
<dd>{@render children()}</dd>
