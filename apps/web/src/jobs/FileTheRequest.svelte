<script lang="ts">
  // "File the request" on a rendered plan's own page (backlog 7aef2405).
  //
  // A plan-* preview packet that rendered clean files its mutating write
  // from here: the jobs API reads the write, host and args off the plan
  // and files it as the signed-in viewer, and the filed request waits on
  // its approve step for the verb's approvers' passkey. Filing is not
  // approving, and the panel says so before the button.
  //
  // Data-keyed, like GateTimes (CLAUDE.md §9): asked of any packet that
  // carries an ops request's verb and host, and drawn only when the
  // server reads it as a plan. A failed read says it failed; it is not
  // drawn as "nothing to file".
  import Section from '@boss/web-kit/ui/Section.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import {
    carriesAnOpsRequest,
    fileTheRequest,
    loadPlanPreview,
    waitsOn,
    type Filed,
    type PlanPreview,
  } from './fileTheRequest';

  type Props = Readonly<{
    jobId: string;
    metadata: Readonly<Record<string, unknown>> | undefined;
  }>;
  let { jobId, metadata }: Props = $props();

  const asks = $derived(carriesAnOpsRequest(metadata));

  type Reading = { kind: 'ready'; data: PlanPreview } | { kind: 'failed'; error: string };
  let reading = $state<Reading | null>(null);
  let filing = $state(false);
  let filed = $state<Filed | null>(null);

  $effect(() => {
    if (!asks) return;
    const id = jobId;
    reading = null;
    filed = null;
    void loadPlanPreview(id).then((r) => {
      if (id === jobId) reading = r;
    });
  });

  async function file(): Promise<void> {
    if (filing) return;
    filing = true;
    const id = jobId;
    const answer = await fileTheRequest(id);
    if (id !== jobId) return;
    filed = answer;
    filing = false;
    // Re-read: a filed request makes the reading name it, so a second
    // press is not offered.
    reading = await loadPlanPreview(id);
  }
</script>

{#if asks && reading !== null && !(reading.kind === 'ready' && reading.data.kind === 'not-a-plan')}
  <Section title="File the request">
    <div class="ftr" data-testid="file-the-request">
      {#if reading.kind === 'failed'}
        <p class="ftr-line ftr-muted load-failed" role="alert">
          Couldn't read whether this plan can file its request: {reading.error}
        </p>
      {:else if reading.data.kind === 'fileable'}
        {@const request = reading.data.request}
        <p class="ftr-line">
          This plan rendered clean. Filing opens <code class="ftr-mono">{request.title}</code> — the
          {request.mutating ? 'MUTATING ' : ''}write it previewed, with the same arguments — as you.
        </p>
        <p class="ftr-line">{waitsOn(request)} Filing is not approving.</p>
        <WriteGate>
          <button type="button" class="btn btn-primary" disabled={filing} onclick={() => void file()}>
            {filing ? 'Filing…' : 'File the request'}
          </button>
        </WriteGate>
      {:else if reading.data.kind === 'refused'}
        {@const refused = reading.data}
        {#if refused.open_request !== null}
          <p class="ftr-line">
            Filed — the request is open and waits on its own steps:
            <Link to={entityHref('job', refused.open_request)} className="ftr-mono">{refused.open_request}</Link>
          </p>
        {:else}
          <p class="ftr-line ftr-muted">
            {refused.write ? `${refused.write} cannot be filed from this plan` : 'Nothing can be filed from this plan'}:
            {refused.why}
          </p>
        {/if}
      {/if}
      {#if filed?.kind === 'refused'}
        <p class="ftr-line ftr-refused" role="alert">{filed.why}</p>
      {:else if filed?.kind === 'filed' && !(reading?.kind === 'ready' && reading.data.kind === 'refused' && reading.data.open_request === filed.id)}
        <p class="ftr-line">
          Filed <Link to={entityHref('job', filed.id)} className="ftr-mono">{filed.id}</Link>
        </p>
      {/if}
    </div>
  </Section>
{/if}

<style>
  .ftr { display: grid; gap: 8px; }
  .ftr-line { margin: 0; font-size: 13px; line-height: 1.45; }
  .ftr-muted { color: var(--text-dim); }
  .ftr-refused { color: var(--err); white-space: pre-wrap; }
  :global(.ftr-mono), .ftr-mono { font-family: var(--font-mono); font-size: 12px; overflow-wrap: anywhere; }
</style>
