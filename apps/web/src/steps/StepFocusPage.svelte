<script lang="ts">
  // Full-page step surface.
  //
  // A plugin-backed step normally renders as a panel inside the job
  // page, below the job header and beside the step list. That is right
  // for a checklist and wrong for anything you have to *read*: the
  // design-review surface is a document plus a set of decisions, and it
  // was competing for width with a sidebar, a step list and job chrome.
  //
  // This route gives the step the viewport. It renders OUTSIDE AppShell
  // (App.svelte branches before the shell, as it does for login), so
  // there is no sidebar — only the chrome bar above and a slim bar
  // naming the job you came from.
  //
  // Host-side by necessity: a plugin cannot decide how it is mounted.
  // Everything below the header is still the plugin's, unchanged — the
  // same bundle renders here and inline.
  import { onMount } from 'svelte';
  import StepPluginMount from './StepPluginMount.svelte';
  import type { StepPluginProps } from './pluginHost';
  import { navigate } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';

  let { jobId, stepId } = $props<{ jobId: string; stepId: string }>();

  // Reuse the plugin contract's own step shape rather than
  // redeclaring it — a local copy drifts, and the drift only shows up
  // as a type error at the mount site (it did: `notes`, and metadata's
  // optionality).
  type Step = StepPluginProps['step'];
  type Job = { id: string; title: string; kind: string; status: string };

  let job = $state<Job | null>(null);
  let step = $state<Step | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // The plugin contract takes `PluginCurrentUser | undefined`, not
  // null — undefined means "no user known", which is what a
  // not-yet-ready session is.
  let currentUser = $derived(
    session.value.kind === 'ready'
      ? { id: session.value.user.id, role: session.value.user.role }
      : undefined,
  );

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const jr = await fetch(`/api/jobs/${jobId}`);
      if (!jr.ok) throw new Error(`job: HTTP ${jr.status}`);
      const body = await jr.json();
      job = body as Job;
      const steps: Step[] = Array.isArray(body.steps) ? body.steps : [];
      const found = steps.find((s) => s.id === stepId) ?? null;
      if (!found) throw new Error(`step ${stepId} is not part of this job`);
      step = found;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<div class="step-focus">
  <div class="step-focus-bar">
    <button class="step-focus-back" onclick={() => navigate(`/ux/jobs/${jobId}`)}>
      ← {job?.title ?? 'Back to job'}
    </button>
    {#if step}
      <span class="step-focus-title">{step.title}</span>
      <span class="step-focus-status">{step.status}</span>
    {/if}
  </div>

  <div class="step-focus-body">
    {#if loading}
      <p class="step-focus-msg">Loading step…</p>
    {:else if error}
      <p class="step-focus-msg step-focus-err">{error}</p>
    {:else if step}
      <StepPluginMount
        kind={step.kind}
        {step}
        {jobId}
        {currentUser}
        onUpdate={load}
      />
    {/if}
  </div>
</div>

<style>
  /* Offset below the fixed 44px chrome bar, then take everything. */
  .step-focus {
    position: absolute;
    inset: 44px 0 0 0;
    display: flex;
    flex-direction: column;
    background: var(--bg, #fafaf9);
  }
  .step-focus-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 24px;
    border-bottom: 1px solid var(--border, #e7e5e4);
    background: var(--card, #fff);
  }
  .step-focus-back {
    background: none;
    border: none;
    padding: 4px 6px;
    margin-left: -6px;
    border-radius: 4px;
    cursor: pointer;
    font: inherit;
    font-size: 13px;
    color: var(--text-dim, #78716c);
  }
  .step-focus-back:hover {
    background: var(--bg, #f5f5f4);
    color: var(--text, #1c1917);
  }
  .step-focus-title {
    font-size: 14px;
    font-weight: 600;
    flex: 1 1 auto;
  }
  .step-focus-status {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim, #78716c);
  }
  /* The plugin owns everything from here down. */
  .step-focus-body {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 24px;
  }
  /* Let a two-pane plugin fill the viewport instead of scrolling
     inside its own 78vh box inside this one — two nested scroll
     regions is exactly the cramped feeling this route exists to fix.
     Scoped to the pane classes, so any other plugin just scrolls the
     body normally. */
  .step-focus-body :global(.step-review-design .srd-doc),
  .step-focus-body :global(.step-review-design .srd-rail) {
    max-height: calc(100vh - 190px);
  }
  .step-focus-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .step-focus-err {
    color: #b91c1c;
  }
</style>
