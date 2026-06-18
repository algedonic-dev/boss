<script lang="ts">
  // `tier` is nullable — identity-first: an account can exist before it
  // is classified into a tier. Render an "untiered" chip until it is.
  let { tier } = $props<{ tier: 'platinum' | 'gold' | 'silver' | null }>();
</script>

{#if tier}
  <span class="chip chip-tier chip-tier-{tier}">{tier}</span>
{:else}
  <span class="chip chip-tier chip-tier-none">untiered</span>
{/if}
