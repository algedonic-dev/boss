<script lang="ts">
  // The packet's case for action, rendered above every non-plugin
  // step surface (19db52de). A sign-off button with no context is not
  // a choice; this panel is the default that makes one. Plugins are
  // exempt — a mounted plugin IS the bespoke presentation.
  //
  // Text renders whitespace-preserved rather than through a markdown
  // pipeline — the SPA has no markdown renderer today, and the review
  // plugin's raw fallback proved readable. Swap the interior when one
  // lands; the resolution chain stays.
  import { contextFromJob, contextFromStep } from './decisionContext';
  import type { DecisionContext } from './decisionContext';

  type Props = {
    step: { id: string; metadata: Record<string, unknown> };
    jobId: string;
  };
  let { step, jobId }: Props = $props();

  let resolved = $state<DecisionContext | null>(null);
  let collapsed = $state(false);

  $effect(() => {
    const own = contextFromStep(step.metadata);
    if (own) {
      resolved = own;
      return;
    }
    resolved = null;
    let cancelled = false;
    void (async () => {
      try {
        const r = await fetch(`/api/jobs/${jobId}`, {
          headers: { accept: 'application/json' },
        });
        if (!r.ok || cancelled) return;
        const job = (await r.json()) as { metadata?: Record<string, unknown> };
        if (cancelled) return;
        resolved = contextFromJob(job.metadata ?? {});
      } catch {
        // No context is a quiet absence, never a broken surface.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  const sourceLabel: Record<DecisionContext['source'], string> = {
    step: 'written for this step',
    'job-context': 'the packet’s briefing',
    'job-message': 'the packet as filed',
  };
</script>

{#if resolved}
  <div class="step-decision-context">
    <button
      type="button"
      class="sdc-head"
      onclick={() => (collapsed = !collapsed)}
    >
      <span class="sdc-title">What this step is deciding</span>
      <span class="sdc-source">{sourceLabel[resolved.source]}</span>
      <span class="sdc-toggle">{collapsed ? 'show' : 'hide'}</span>
    </button>
    {#if !collapsed}
      <div class="sdc-body">{resolved.text}</div>
    {/if}
  </div>
{/if}

<style>
  .step-decision-context {
    border: 1px solid var(--border, #e7e5e4);
    border-left: 3px solid var(--accent, #2563eb);
    border-radius: 6px;
    background: var(--card, #fff);
    margin-bottom: 12px;
  }
  .sdc-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: 0;
    cursor: pointer;
    text-align: left;
  }
  .sdc-title {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-dim, #78716c);
  }
  .sdc-source {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    flex: 1 1 auto;
  }
  .sdc-toggle {
    font-size: 11px;
    color: var(--accent, #2563eb);
  }
  .sdc-body {
    padding: 0 12px 10px;
    font-size: 13px;
    line-height: 1.6;
    color: var(--text, #1c1917);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 22em;
    overflow-y: auto;
  }
</style>
