<script lang="ts">
  // Approval / sign-off surface — port of
  // apps/web-legacy/src/steps/ApprovalSurface.tsx.

  import { session } from '../session/session.svelte';
  import { appNow, appToday } from '../shell/sim-clock.svelte';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: string;
    metadata: Record<string, unknown>;
    notes: string | null;
    sign_offs_required?: string[];
    sign_offs?: { role: string; authority_id: string; shape_hash: string }[];
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  let comment = $state(String(step.metadata.comment ?? ''));
  let saving = $state(false);

  let decision = $derived(String(step.metadata.decision ?? 'pending'));
  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );
  let userRole = $derived(
    session.value.kind === 'ready' ? session.value.user.role : '',
  );
  let signError = $state('');

  async function decide(d: string): Promise<void> {
    saving = true;
    try {
      const body: Record<string, unknown> = {
        ...step,
        job_id: jobId,
        // v2: both approve and reject COMPLETE the step. The reject
        // decision lives in metadata.decision; downstream routing is
        // predicate-driven server-side (no client-set 'blocked').
        status: step.status,
        metadata: {
          ...step.metadata,
          decision: d,
          decided_at: appNow().toISOString(),
          comment: comment || undefined,
        },
      };
      // Sign-off contract: a stamp attests the step's current shape, so the
      // decision lands first, then the stamp, then the completion.
      // The server 409s a completion whose required stamps are
      // missing or stale — surfaced inline below.
      signError = '';
      await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      const required = step.sign_offs_required ?? [];
      if (required.includes(userRole)) {
        await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role: userRole }),
        });
      }
      if (d === 'approved' || d === 'rejected') {
        const done = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status: 'completed' }),
        });
        if (done.status === 409) {
          const conflict = await done.json().catch(() => null);
          const missing = conflict?.missing_or_stale_roles ?? [];
          signError = `sign-offs outstanding: ${missing.join(', ')}`;
        }
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-approval">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  {#if signError}
    <p class="step-approval-error">{signError}</p>
  {/if}
  {#if decision !== 'pending' && decision !== ''}
    <div class="step-approval-result step-approval-{decision}">
      Decision: <strong>{decision}</strong>
      {#if comment}<div class="step-approval-comment">{comment}</div>{/if}
    </div>
  {:else}
    <div class="step-approval-form">
      <div class="step-field">
        <label for={`approval-comment-${step.id}`}>Comment (optional)</label>
        <textarea
          id={`approval-comment-${step.id}`}
          rows="2"
          bind:value={comment}
          placeholder="Add a comment..."
        ></textarea>
      </div>
      <div class="step-actions">
        <button
          class="step-btn step-btn-approve"
          onclick={() => decide('approved')}
          disabled={saving}
        >
          Approve
        </button>
        <button
          class="step-btn step-btn-reject"
          onclick={() => decide('rejected')}
          disabled={saving}
        >
          Reject
        </button>
        <button
          class="step-btn"
          onclick={() => decide('changes-requested')}
          disabled={saving}
        >
          Request changes
        </button>
      </div>
    </div>
  {/if}
</div>
