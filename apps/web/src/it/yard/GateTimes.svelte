<script lang="ts">
  // A gate-run's two ages on its own packet page (backlog 4d088a7e):
  // how long it waited in line and how long its Job has run, with the
  // median running time beside them. Gate-run 6d5d85fb waited 46
  // minutes and ran 14, the page showed only the packet's age, and the
  // answer to "what is the gate that has been going for an hour doing"
  // took kubectl.
  //
  // Data-keyed, not kind-keyed (CLAUDE.md §9): the panel renders because
  // the packet carries a gate lifecycle stamp (`launched_at` or
  // `queued_at`, both written by `boss gate`), not because some branch
  // says `kind === 'gate-run'`. An open run is read off the yard's own
  // reading of the bays, so the page and the floor judge it with one
  // definition; a settled one off its own stamps.
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { fetchYardStatus, gateStanding, settledGateTimes, type GateStanding } from './yard-status';

  type Props = Readonly<{
    jobId: string;
    open: boolean;
    metadata: Readonly<Record<string, unknown>> | undefined;
  }>;
  let { jobId, open, metadata }: Props = $props();

  const stamped = $derived(
    typeof metadata?.launched_at === 'string' || typeof metadata?.queued_at === 'string',
  );
  const settled = $derived(open ? null : settledGateTimes(metadata));

  type Live = { kind: 'standing'; at: GateStanding | null } | { kind: 'failed'; error: string };
  let live = $state<Live | null>(null);
  $effect(() => {
    if (!stamped || !open) return;
    const id = jobId;
    void (async () => {
      const r = await fetchYardStatus();
      if (id !== jobId) return;
      live =
        r.kind === 'ready'
          ? { kind: 'standing', at: gateStanding(r.data.gates, id) }
          : { kind: 'failed', error: r.kind === 'failed' ? r.error : 'the yard read did not answer' };
    })();
  });
</script>

{#if stamped && (settled !== null || (open && live !== null))}
  <Section title="At the gates">
    {#if settled !== null}
      <p class="gt-line">{settled}</p>
    {:else if live?.kind === 'failed'}
      <p class="gt-line gt-muted">times unavailable — {live.error}</p>
    {:else if live?.kind === 'standing' && live.at !== null}
      <p class="gt-line">
        {live.at.kind === 'running' ? 'in a gate bay' : 'waiting for a gate bay'} · {live.at.text}
      </p>
      {#if live.at.troubled}
        <p class="gt-line gt-trouble">TROUBLED — running past 2× the median running time</p>
      {/if}
    {:else}
      <p class="gt-line gt-muted">not in a gate bay or the line on the yard's last reading</p>
    {/if}
  </Section>
{/if}

<style>
  .gt-line {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  .gt-muted {
    color: var(--text-dim);
  }
  .gt-trouble {
    color: var(--err);
    font-weight: 600;
  }
</style>
