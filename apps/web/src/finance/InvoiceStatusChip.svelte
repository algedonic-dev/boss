<script lang="ts">
  import { INVOICE_STATUS_LABEL, type InvoiceStatus } from './types';

  type Props = { status: InvoiceStatus };
  let { status }: Props = $props();
  let variant = $derived(
    status === 'paid' ? 'ok' : status === 'past-due' ? 'warn' : 'muted',
  );
  let label = $derived(INVOICE_STATUS_LABEL[status]);
</script>

<span class="chip chip-stage chip-stage-{variant}">{label}</span>
