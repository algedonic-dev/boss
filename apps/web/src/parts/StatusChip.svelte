<script lang="ts">
  import { STATUS_LABEL, type StockStatus } from './types';
  type Props = { status: StockStatus };
  let { status }: Props = $props();
  let label = $derived(STATUS_LABEL[status]);
</script>

<span class="chip chip-stock chip-stock-{status}">{label}</span>
