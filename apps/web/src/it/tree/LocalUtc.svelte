<script lang="ts">
  // ONE INSTANT, TWO CLOCKS — every absolute time the station-tree floor
  // and the station page draw. David, user-feedback 0b1fd59f
  // (2026-11-08): "We need to show UTC in addition to local time since
  // the factory runs on UTC." Design b03ef2d4: `17:42 PT · 01:42Z`, the
  // UTC half secondary ink on the same line, both halves from the SPA's
  // one date formatter (@boss/web-kit/ui/date.ts) — no page formats its
  // own. Relative ages are not instants and do not come through here.
  import { formatLocalAndUtc, localAndUtcText } from '@boss/web-kit/ui/date';

  let { at, seconds = false, date = false }: Readonly<{ at: string; seconds?: boolean; date?: boolean }> = $props();

  const pair = $derived(formatLocalAndUtc(at, { seconds, date }));
</script>

{#if pair !== null}<time datetime={at} title={localAndUtcText(pair)} data-clock>{pair.local} <span class="utc" data-clock-utc>· {pair.utc}</span></time>{:else}{at}{/if}

<style>
  time { white-space: nowrap; }
  .utc { color: var(--static); }
</style>
