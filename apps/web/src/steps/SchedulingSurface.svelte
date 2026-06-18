<script lang="ts">
  // Scheduling step surface — slot a Job into a calendar window.
  // The morning-brew JobKind opens with a scheduling step
  // ("plan today's brew"), wholesale-keg-order's tier 2 schedules
  // the production batch, equipment-preventive-maintenance schedules the technician
  // visit.
  //
  // Demo framing: pick a date/time + duration + assignee, save.
  // The full calendar conflict view is overkill for the surface;
  // the Calendar KB page in the sidebar handles that.

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

  function pickStr(key: string, fallback = ''): string {
    const v = step.metadata[key];
    return typeof v === 'string' ? v : fallback;
  }
  function pickNum(key: string): number | '' {
    const v = step.metadata[key];
    return typeof v === 'number' ? v : '';
  }

  let location = $state(pickStr('location'));
  let scheduledAt = $state(pickStr('scheduled_at'));
  let durationMinutes = $state<number | ''>(pickNum('duration_minutes'));
  let assigneeId = $state(step.assignee_id ?? '');
  let notes = $state(step.notes ?? '');
  let saving = $state(false);
  let terminal = $derived(_isTerminal(step.status));

  let employees = $state<Employee[]>([]);

  $effect(() => {
    let cancelled = false;
    fetch('/api/people')
      .then((r) => (r.ok ? r.json() : []))
      .then((roster: Employee[]) => {
        if (!cancelled) employees = roster;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });



  // <input type="datetime-local"> wants `YYYY-MM-DDTHH:mm`. The
  // metadata stores ISO-8601 with possible seconds + timezone, so
  // trim down for the input + restore on save.
  let scheduledAtForInput = $derived(scheduledAt.slice(0, 16));

  async function persist(status?: string): Promise<void> {
    saving = true;
    try {
      const body = {
        ...step,
        job_id: jobId,
        notes: notes || undefined,
        status: status ?? step.status,
        assignee_id: assigneeId || null,
        metadata: {
          ...step.metadata,
          location: location || undefined,
          scheduled_at: scheduledAt || undefined,
          duration_minutes:
            typeof durationMinutes === 'number' ? durationMinutes : undefined,
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

<div class="step-surface step-scheduling">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-field-row">
    <div class="step-field step-assign-row">
      <label for={`when-${step.id}`}>When</label>
      <input
        id={`when-${step.id}`}
        type="datetime-local"
        value={scheduledAtForInput}
        disabled={terminal || saving}
        oninput={(e) => (scheduledAt = (e.target as HTMLInputElement).value)}
      />
    </div>
    <div class="step-field step-assign-row step-duration">
      <label for={`dur-${step.id}`}>Duration (min)</label>
      <input
        id={`dur-${step.id}`}
        type="number"
        min="0"
        step="15"
        value={durationMinutes}
        disabled={terminal || saving}
        oninput={(e) => {
          const n = parseInt((e.target as HTMLInputElement).value);
          durationMinutes = Number.isFinite(n) ? n : '';
        }}
      />
    </div>
  </div>

  <div class="step-field step-assign-row">
    <label for={`loc-${step.id}`}>Location</label>
    <input
      id={`loc-${step.id}`}
      type="text"
      bind:value={location}
      disabled={terminal || saving}
      placeholder="e.g. loc-brewery-brewhouse"
    />
  </div>

  <div class="step-field step-assign-row">
    <label for={`assignee-${step.id}`}>Assignee</label>
    <select
      id={`assignee-${step.id}`}
      bind:value={assigneeId}
      disabled={terminal || saving}
    >
      <option value="">— unassigned —</option>
      {#each employees as e (e.id)}
        <option value={e.id}>{e.name} · {e.role}</option>
      {/each}
    </select>
  </div>

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Window constraints, dependencies..."
      disabled={terminal}
    ></textarea>
  </div>

  <div class="step-actions">
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('active')}
        disabled={saving || !scheduledAt}
        title={!scheduledAt ? 'Pick a date/time first' : ''}
      >
        Schedule
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('completed')}
        disabled={saving || !scheduledAt}
      >
        {saving ? 'Saving…' : 'Confirm'}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-field-row {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }
  .step-field-row .step-field {
    flex: 1;
    min-width: 130px;
  }
  .step-duration {
    flex: 0 0 140px;
  }
</style>
