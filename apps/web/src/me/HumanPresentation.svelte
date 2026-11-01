<script lang="ts">
  import { safeLinkHref } from '@boss/web-kit/links';
  import { humanPresentation } from './humanPresentation';
  import { stepFocusHref } from './decideHref';

  let { metadata, jobId, stepId, jobTitle, stepTitle, onOpen }: {
    metadata?: Record<string, unknown> | null;
    jobId: string;
    stepId: string;
    jobTitle?: string;
    stepTitle?: string;
    onOpen?: () => void;
  } = $props();
  const view = $derived(humanPresentation(metadata));
</script>

{#if view.kind === 'ready'}
  <section class="human-ask" aria-label="What needs you">
    {#if jobTitle}<p><strong>{jobTitle}</strong></p>{/if}
    {#if stepTitle}<p>{stepTitle}</p>{/if}
    <h3>{view.presentation.ask}</h3>
    <p>{view.presentation.why_person}</p>
    <p><strong>Afterwards:</strong> {view.presentation.after}</p>
    <a href={safeLinkHref(stepFocusHref(jobId, stepId))}>Read the evidence and full instructions</a>
    {#if onOpen}
      <button type="button" onclick={onOpen}>{view.presentation.action}</button>
    {:else}
      <p><strong>Your action:</strong> {view.presentation.action}</p>
    {/if}
  </section>
{/if}

<style>
  .human-ask { padding: 14px; border: 1px solid var(--hairline); border-radius: 4px; }
  h3 { margin: 0 0 8px; font-size: 1rem; }
  p { margin: 8px 0; }
  button { display: block; margin-top: 12px; }
</style>
