<script lang="ts">
  // Generic step surface — fallback for kinds without a specialised
  // view. Doubles as the "assign tech / reschedule" affordance that
  // every service Job's steps pick up implicitly. Port of
  // apps/web-legacy/src/steps/GenericSurface.tsx.

  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import type { Employee } from '../people/types';

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

  const initialDueOn =
    typeof step.metadata.due_on === 'string' ? step.metadata.due_on : '';

  let notes = $state(step.notes ?? '');
  let assigneeId = $state(step.assignee_id ?? '');
  let dueOn = $state(initialDueOn);
  let saving = $state(false);
  let terminal = $derived(_isTerminal(step.status));

  let employees = $state<Employee[]>([]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const roster = (await r.json()) as Employee[];
          if (!cancelled) employees = roster;
        }
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let empNames = $derived.by(() => {
    const m = new Map<string, string>();
    for (const e of employees) m.set(e.id, e.name ?? "");
    return m;
  });


  let assigneeDirty = $derived(
    (assigneeId || null) !== (step.assignee_id ?? null),
  );
  let dueOnDirty = $derived((dueOn || '') !== initialDueOn);
  let dirty = $derived(assigneeDirty || dueOnDirty);

  let activeEmployees = $derived(
    [...employees].sort((a, b) => (a.name ?? "").localeCompare(b.name ?? "")),
  );

  function mergeMetadata(
    existing: Record<string, unknown>,
    d: string,
  ): Record<string, unknown> {
    const next = { ...existing };
    if (d) next.due_on = d;
    else delete next.due_on;
    return next;
  }

  async function persist(overrides: {
    status?: string;
    assignee_id?: string | null;
    metadata?: Record<string, unknown>;
    notes?: string;
  }): Promise<void> {
    saving = true;
    try {
      const body = {
        ...step,
        job_id: jobId,
        notes: overrides.notes ?? notes ?? undefined,
        status: overrides.status ?? step.status,
        assignee_id:
          overrides.assignee_id !== undefined
            ? overrides.assignee_id
            : assigneeId || null,
        metadata: overrides.metadata ?? mergeMetadata(step.metadata, dueOn),
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

  let extraMetadataEntries = $derived(
    Object.entries(step.metadata).filter(([k]) => k !== 'due_on'),
  );
</script>

<div class="step-surface step-generic">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-field step-assign-row">
    <label for={`assignee-${step.id}`}>Assignee</label>
    <select
      id={`assignee-${step.id}`}
      bind:value={assigneeId}
      disabled={terminal || saving}
    >
      <option value="">— unassigned —</option>
      {#each activeEmployees as e (e.id)}
        <option value={e.id}>{e.name} · {e.role}</option>
      {/each}
    </select>
    {#if step.assignee_id && !assigneeDirty}
      <span class="step-meta-row small">
        ({empNames.get(step.assignee_id) ?? step.assignee_id})
      </span>
    {/if}
  </div>

  <div class="step-field step-assign-row">
    <label for={`due-${step.id}`}>Due on</label>
    <input
      id={`due-${step.id}`}
      type="date"
      bind:value={dueOn}
      disabled={terminal || saving}
    />
  </div>

  {#if extraMetadataEntries.length > 0}
    <div class="step-metadata-display">
      {#each extraMetadataEntries as [k, v] (k)}
        <div class="step-meta-row">
          <strong>{k}:</strong>
          {typeof v === 'object' ? JSON.stringify(v) : String(v)}
        </div>
      {/each}
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Add notes..."
      disabled={terminal}
    ></textarea>
  </div>

  <div class="step-actions">
    {#if dirty && !terminal}
      <button
        class="step-btn"
        onclick={() => persist({})}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save assignment'}
      </button>
    {/if}
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist({ status: 'active' })}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist({ status: 'completed' })}
        disabled={saving}
      >
        Complete
      </button>
    {/if}
  </div>
</div>
