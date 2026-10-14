<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import { navigate } from '../../router';
  import type { Remote } from '../../data/remote';
  import type { StationQueueView } from '../monitoring/stationMap';
  import { parseStationQueue } from './stationQueue';

  let { station, onclose }: Readonly<{ station: string; onclose: () => void }> = $props();
  let queue = $state<Remote<StationQueueView>>({ kind: 'loading' });
  // The parent keys this panel by station. Closing or choosing another
  // station retires this read; its late answer cannot paint the new one.
  let alive = true;
  onDestroy(() => { alive = false; });
  onMount(async () => {
    try {
      const response = await fetch(`/api/stations/${encodeURIComponent(station)}/queue`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = parseStationQueue(await response.json(), station);
      if (alive) queue = { kind: 'ready', data };
    } catch (error) {
      if (alive) queue = { kind: 'failed', error: error instanceof Error ? error.message : String(error) };
    }
  });
</script>

<section class="station-queue" aria-label="Queue: {station}">
  <div class="queue-head"><h3>{station}</h3><button type="button" onclick={onclose}>Close queue</button></div>
  {#if queue.kind === 'loading'}
    <p>Reading the queue…</p>
  {:else if queue.kind === 'failed'}
    <p class="load-failed" role="alert">Queue unknown — {queue.error}. This read does not show an empty queue.</p>
  {:else}
    <p>{queue.data.total} packets · order: {queue.data.discipline}</p>
    {#if queue.data.cards.length === 0 && queue.data.total === 0}
      <p>This station's queue is empty.</p>
    {:else}
      <div class="queue-packets">
        {#each queue.data.cards as card (card.id)}
          <PacketCard {card} onOpen={(id) => navigate(`/jobs/${id}`)} />
        {/each}
      </div>
      {#if queue.data.cards.length < queue.data.total}
        <p>{queue.data.cards.length} of {queue.data.total} packets returned — the remaining packets were not read.</p>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .station-queue { border: 1px solid var(--hairline); padding: 12px; margin: 12px 0; }
  .queue-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  h3 { font: 600 14px var(--font-mono); margin: 0; }
  p { color: var(--static); font-size: 13px; }
  button { font: inherit; color: inherit; background: transparent; border: 1px solid var(--hairline); cursor: pointer; }
  .queue-packets { display: grid; gap: 8px; }
</style>
