<script lang="ts">
  // Resolve a fact's (source_table, source_id) → its single journal
  // entry + lines. Used by the IT-panel activity tabs.

  import { loadEntryBySource, type LedgerEntryDetail } from './ledger';
  import EntryDetailPanel from './EntryDetailPanel.svelte';

  type Props = {
    sourceTable: string | null;
    sourceId: string | null;
  };
  let { sourceTable, sourceId }: Props = $props();

  let entry = $state<LedgerEntryDetail | null>(null);
  let loading = $state(false);

  $effect(() => {
    const t = sourceTable;
    const i = sourceId;
    if (!t || !i) {
      entry = null;
      return;
    }
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadEntryBySource(t, i);
      if (!cancelled) {
        entry = d;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });
</script>

<EntryDetailPanel {entry} {loading} />
