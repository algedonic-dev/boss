<script lang="ts">
  // Inspection step — structured QA check surface. Two columns:
  // result + inspector notes on the left, system model + preventive maintenance
  // checklist (pulled from the catalog KB) on the right.
  //
  // If the step requires sign-offs, completion stamps land first via
  // POST .../sign-offs (the sign-off contract) and the server gates the flip
  // from the session user. The backend policy check rejects
  // unauthorized callers.

  import { isPending, type StepStatus } from '../jobs/types';
  import EntityLink from '../ui/EntityLink.svelte';
  import Section from '../ui/Section.svelte';
  import { session } from '../session/session.svelte';
  import { appToday } from '../shell/sim-clock.svelte';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
    sign_offs_required?: string[];
    sign_offs?: { role: string; shape_hash: string }[];
  };

  type CatalogModel = {
    sku: string;
    name: string;
    service?: {
      pm_checklist?: string[];
      required_skill_level?: number;
    };
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  const assetId = (step.metadata.asset_id as string | undefined) ?? '';

  let result = $state<string>(String(step.metadata.overall_result ?? ''));
  let notes = $state<string>(String(step.metadata.inspector_notes ?? ''));
  let saving = $state(false);

  let sku = $state<string | null>(null);
  let model = $state<CatalogModel | null>(null);

  $effect(() => {
    if (!assetId) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(`/api/assets/${encodeURIComponent(assetId)}`);
        if (!r.ok || cancelled) return;
        const data = await r.json();
        if (!cancelled) sku = data.current_state?.sku ?? null;
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const s = sku;
    if (!s) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/catalog/models');
        if (!r.ok || cancelled) return;
        const rows = (await r.json()) as CatalogModel[];
        const match = rows.find((m) => m.sku === s) ?? null;
        if (!cancelled) model = match;
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let pmChecklist = $derived<string[]>(model?.service?.pm_checklist ?? []);
  let currentUserId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );

  async function save(newStatus?: string): Promise<void> {
    saving = true;
    try {
      const body: Record<string, unknown> = {
        ...step,
        job_id: jobId,
        metadata: {
          ...step.metadata,
          overall_result: result || undefined,
          inspector_notes: notes || undefined,
        },
      };
      const required = step.sign_offs_required ?? [];
      const completing = newStatus === 'completed' && required.length > 0;
      if (newStatus && !completing) body.status = newStatus;
      // Metadata first, then stamps attesting the final shape, then
      // the status flip. Server gates the completion.
      await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (completing) {
        const myRole =
          session.value.kind === 'ready' ? session.value.user.role : '';
        if (required.includes(myRole)) {
          await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ role: myRole }),
          });
        }
        await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status: 'completed' }),
        });
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-inspection">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">inspection</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
    {#if step.assignee_id}
      <span class="step-assignee">
        Assigned:
        <EntityLink kind="employee" id={step.assignee_id} />
      </span>
    {/if}
  </div>

  <div class="step-repair-layout">
    <div class="step-repair-form">
      <Section title="Inspection">
          <div class="step-field">
            <label for="insp-result-{step.id}">Overall result</label>
            <select
              id="insp-result-{step.id}"
              bind:value={result}
              disabled={saving}
            >
              <option value="">— Pending —</option>
              <option value="pass">Pass</option>
              <option value="fail">Fail</option>
              <option value="conditional">Conditional</option>
            </select>
          </div>

          <div class="step-field">
            <label for="insp-notes-{step.id}">Inspector notes</label>
            <textarea
              id="insp-notes-{step.id}"
              rows="4"
              bind:value={notes}
              disabled={saving}
              placeholder="Observations, measurements, anomalies..."
            ></textarea>
          </div>
      </Section>

      <div class="step-actions">
        {#if isPending(step.status)}
          <button
            class="step-btn step-btn-primary"
            onclick={() => save('active')}
            disabled={saving}
          >Start inspection</button>
        {:else if step.status === 'active'}
          <button class="step-btn" onclick={() => save()} disabled={saving}>
            Save progress
          </button>
          <button
            class="step-btn step-btn-primary"
            onclick={() => save('completed')}
            disabled={saving || !result}
          >Complete inspection</button>
        {/if}
      </div>
    </div>

    <div class="step-repair-context">
      {#if model}
        <Section title="System model">
            <div class="step-kb-card">
              <div class="step-kb-row">
                <strong>Model:</strong> {model?.name ?? '—'}
              </div>
              <div class="step-kb-row">
                <strong>SKU:</strong> {sku ?? '—'}
              </div>
              {#if model?.service?.required_skill_level}
                <div class="step-kb-row">
                  <strong>Skill:</strong> {model.service.required_skill_level}/5
                </div>
              {/if}
            </div>
        </Section>
      {/if}

      {#if pmChecklist.length > 0}
        <Section title="preventive maintenance checklist (from KB)">
            <div class="step-kb-checklist">
              {#each pmChecklist as item, i (i)}
                <div class="step-kb-checklist-item">
                  <span class="step-kb-check">☐</span>
                  {item}
                </div>
              {/each}
            </div>
        </Section>
      {/if}
    </div>
  </div>
</div>
