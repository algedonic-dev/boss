<script lang="ts">
  // One registry row's packet history, from GET /api/jobs/kinds
  // (backlogs 112c0535 and 5eacf6db): how many of its in-flight packets
  // run a version below the active one, how many packets it has ever
  // had and its newest terminal — or that it has never run, marked,
  // because a protocol nobody runs is where the operating model drifts
  // from reality unseen. Renders nothing when the ledger was not read:
  // the page says that once, and unknown is not "never run".

  import Link from '@boss/web-kit/ui/Link.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import { href } from '../../router';
  import type { KindHistory } from './kindLedger';

  let { history }: Readonly<{ history: KindHistory | null }> = $props();

  const plural = (n: number, one: string, many: string): string => `${n} ${n === 1 ? one : many}`;
</script>

{#if history?.kind === 'never-run'}
  <div class="kb-ledger">
    <span class="kb-ledger-never">never run</span>
    no packet of this kind has ever been opened
  </div>
{:else if history?.kind === 'ran'}
  <div class="kb-ledger">
    {#if history.inFlight > 0}
      {history.inFlight} in flight,
      {#if history.older > 0}
        <span
          class="kb-ledger-behind"
          title={history.olderVersions.map(([v, n]) => `v${v}: ${n}`).join(' · ')}
        >{history.older} on older versions</span>
        ({history.olderVersions.map(([v, n]) => `v${v}: ${n}`).join(', ')})
      {:else}
        all on the active version
      {/if}
      ·
    {/if}
    {plural(history.packets, 'packet', 'packets')} ever ·
    {#if history.newest}
      newest closed
      <Link to={href(`/ux/jobs/${encodeURIComponent(history.newest.id)}`)}>
        {history.newest.closed_on ? formatDate(history.newest.closed_on) : 'on no recorded date'}
      </Link>{#if history.newest.outcome}&nbsp;({history.newest.outcome}){/if}
    {:else}
      none closed yet
    {/if}
  </div>
{/if}

<style>
  .kb-ledger {
    color: var(--static);
    font-size: 12px;
    margin-top: 4px;
  }
  .kb-ledger-behind {
    color: var(--warn);
    background: var(--warn-wash);
    padding: 1px 6px;
    border-radius: 999px;
    font-weight: 500;
  }
  .kb-ledger-never {
    color: var(--static);
    border: 1px dashed var(--hairline);
    padding: 1px 6px;
    border-radius: 999px;
    font-weight: 500;
    margin-right: 4px;
  }
</style>
