<script lang="ts">
  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import { appToday } from '../shell/sim-clock.svelte';
  // Receiving step surface — three-way match for inbound goods
  // (PO line + actual qty received + over/short delta). The
  // ingredient-restock JobKind opens with a procurement step
  // (places PO) and ends with a receiving step where the
  // warehouse clerk confirms what actually showed up.
  //
  // Demo framing: expected_items arrives populated from the PO
  // (mirrors what the procurement step set). The clerk types
  // actual qty received per line; the surface highlights any
  // discrepancy. On done, the inventory.receive side-effect
  // increments on_hand by received_qty.

  type ExpectedItem = {
    part_sku: string;
    qty: number;
    description?: string;
    received_qty?: number;
  };

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  function asItems(v: unknown): ExpectedItem[] {
    if (!Array.isArray(v)) return [];
    return v
      .filter((x): x is Record<string, unknown> => typeof x === 'object' && x !== null)
      .map((x) => ({
        part_sku: typeof x.part_sku === 'string' ? x.part_sku : '',
        qty: typeof x.qty === 'number' ? x.qty : 0,
        description:
          typeof x.description === 'string' ? x.description : undefined,
        received_qty:
          typeof x.received_qty === 'number' ? x.received_qty : undefined,
      }));
  }

  let items = $state<ExpectedItem[]>(asItems(step.metadata.expected_items));
  let poId = $derived(
    typeof step.metadata.po_id === 'string' ? step.metadata.po_id : null,
  );
  let receivedDate = $state(
    typeof step.metadata.received_date === 'string'
      ? step.metadata.received_date
      : appToday(),
  );
  let discrepancyNotes = $state(
    typeof step.metadata.discrepancy_notes === 'string'
      ? step.metadata.discrepancy_notes
      : '',
  );
  let notes = $state(step.notes ?? '');
  let saving = $state(false);
  let terminal = $derived(_isTerminal(step.status));




  function updateReceived(idx: number, received_qty: number): void {
    items = items.map((it, i) =>
      i === idx ? { ...it, received_qty } : it,
    );
  }

  function delta(it: ExpectedItem): number {
    return (it.received_qty ?? it.qty) - it.qty;
  }
  function deltaClass(it: ExpectedItem): string {
    const d = delta(it);
    if (d === 0) return 'ok';
    if (d > 0) return 'over';
    return 'short';
  }

  let hasDiscrepancy = $derived(items.some((it) => delta(it) !== 0));

  async function persist(status?: string): Promise<void> {
    saving = true;
    try {
      // Default any unset received_qty to expected qty so a clerk
      // who clicks "Confirm receipt" without typing anything per
      // line is treated as "received exactly what was ordered."
      const finalized = items.map((it) => ({
        ...it,
        received_qty: it.received_qty ?? it.qty,
      }));
      const body = {
        ...step,
        job_id: jobId,
        notes: notes || undefined,
        status: status ?? step.status,
        metadata: {
          ...step.metadata,
          expected_items: finalized,
          received_date: receivedDate,
          discrepancy_notes: discrepancyNotes || undefined,
        },
      };
      await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-receiving">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  {#if poId}
    <div class="step-meta-row">
      <strong>PO:</strong> <code class="mono">{poId}</code>
    </div>
  {/if}

  <div class="step-field step-assign-row">
    <label for={`recv-date-${step.id}`}>Received</label>
    <input
      id={`recv-date-${step.id}`}
      type="date"
      bind:value={receivedDate}
      disabled={terminal || saving}
    />
  </div>

  {#if items.length > 0}
    <div class="step-field">
      <label>Lines</label>
      <table class="step-line-items">
        <thead>
          <tr>
            <th class="col-sku">SKU</th>
            <th class="col-desc">Description</th>
            <th class="col-num">Expected</th>
            <th class="col-num">Received</th>
            <th class="col-delta">Δ</th>
          </tr>
        </thead>
        <tbody>
          {#each items as it, idx (idx)}
            <tr>
              <td><code class="mono">{it.part_sku}</code></td>
              <td class="desc">{it.description ?? ''}</td>
              <td class="num">{it.qty}</td>
              <td class="num">
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={it.received_qty ?? ''}
                  placeholder={String(it.qty)}
                  disabled={terminal || saving}
                  oninput={(e) => {
                    const n = parseFloat((e.target as HTMLInputElement).value);
                    if (Number.isFinite(n)) updateReceived(idx, n);
                  }}
                />
              </td>
              <td class="num delta delta-{deltaClass(it)}">
                {#if it.received_qty !== undefined && delta(it) !== 0}
                  {delta(it) > 0 ? '+' : ''}{delta(it)}
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}

  {#if hasDiscrepancy}
    <div class="step-field">
      <label for={`disc-${step.id}`}>Discrepancy notes</label>
      <textarea
        id={`disc-${step.id}`}
        rows="2"
        bind:value={discrepancyNotes}
        placeholder="What's missing / extra / damaged?"
        disabled={terminal}
      ></textarea>
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Receiving exceptions, dock notes..."
      disabled={terminal}
    ></textarea>
  </div>

  <div class="step-actions">
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('active')}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('completed')}
        disabled={saving || items.length === 0}
      >
        {saving ? 'Saving…' : 'Confirm receipt'}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-line-items {
    width: 100%;
    border-collapse: collapse;
    margin-top: 4px;
    font-size: 13px;
  }
  .step-line-items th {
    text-align: left;
    font-weight: 600;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border, #e5e7eb);
    color: var(--text-muted, #6b7280);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .step-line-items td {
    padding: 4px 6px;
    border-bottom: 1px solid var(--border-soft, #f3f4f6);
  }
  .step-line-items th.col-num,
  .step-line-items th.col-delta {
    text-align: right;
  }
  .step-line-items .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    width: 90px;
  }
  .step-line-items .col-sku { width: 180px; }
  .step-line-items .col-delta { width: 60px; }
  .step-line-items input[type="number"] {
    width: 100%;
    box-sizing: border-box;
    padding: 3px 5px;
    font-size: 13px;
    text-align: right;
    border: 1px solid var(--border, #d1d5db);
    border-radius: 3px;
  }
  .step-line-items .desc {
    color: var(--text-muted, #6b7280);
  }
  .step-line-items .delta-ok {
    color: var(--text-muted, #9ca3af);
  }
  .step-line-items .delta-over {
    color: var(--info, #2563eb);
    font-weight: 500;
  }
  .step-line-items .delta-short {
    color: var(--danger, #dc2626);
    font-weight: 500;
  }
</style>
