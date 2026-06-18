<script lang="ts">
  import Link from './Link.svelte';
  let { to, children } = $props<{ to: string; children: () => any }>();
</script>

<Link {to} className="breadcrumb">
  {@render children()}
</Link>
