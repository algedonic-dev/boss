<script lang="ts">
  import type { Snippet } from 'svelte';

  const { name, generation, children }: Readonly<{
    name: string;
    generation: number;
    children: Snippet;
  }> = $props();

  let retry = $state<(() => void) | null>(null);
  let failedAt = 0;

  function captureFailure(_error: unknown, reset: () => void): void {
    failedAt = generation;
    retry = reset;
  }

  $effect(() => {
    if (retry !== null && generation > failedAt) {
      const reset = retry;
      retry = null;
      reset();
    }
  });
</script>

<svelte:boundary onerror={captureFailure}>
  {@render children()}
  {#snippet failed(error)}
    <div class="yard-empty load-failed" data-region={name}>
      The {name} station cannot be drawn — {error instanceof Error ? error.message : String(error)}
    </div>
  {/snippet}
</svelte:boundary>

<style>
  .yard-empty { color: var(--map-muted); padding: 12px 0; font-size: 14px; }
</style>
