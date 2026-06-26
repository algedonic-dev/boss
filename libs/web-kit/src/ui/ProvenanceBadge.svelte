<script lang="ts">
  // Provenance badge — makes the origin of a derived value VISIBLE,
  // never silent. `hand_set` (amber) means a value bootstrapped from a
  // template; `data_derived` (blue) means learned from real performance.
  // Mirrors the chip pattern of apps/web/src/accounts/TierChip.svelte;
  // styles live in apps/web/src/styles.css (.chip-provenance-*).
  let { source }: Readonly<{ source: 'hand_set' | 'data_derived' }> = $props();

  const LABEL = { hand_set: 'hand-set', data_derived: 'data-derived' } as const;
  const VARIANT = { hand_set: 'hand-set', data_derived: 'data-derived' } as const;
</script>

<span class="chip chip-provenance chip-provenance-{VARIANT[source]}">{LABEL[source]}</span>
