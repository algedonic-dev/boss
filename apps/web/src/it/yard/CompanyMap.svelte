<script lang="ts">
  // Approved design d4dada70 (2026-10-02): departments orient the company;
  // only IT has a connected worked example. A stub asserts no health.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate, type Department } from '@boss/web-kit/nav';
  import { fetchRemote, type Remote } from '../../data/remote';
  import { parseCompanyDepartments } from './company-map';

  let reading = $state<Remote<ReadonlyArray<Department>>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);
  let cancelled = false;
  async function refresh(): Promise<void> {
    reading = { kind: 'loading' };
    const next = await fetchRemote('/api/departments', parseCompanyDepartments);
    if (cancelled) return;
    reading = next;
    readAt = Date.now();
  }
  onMount(() => {
    void refresh();
    return () => { cancelled = true; };
  });
  function open(e: MouseEvent): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate('/it?view=overview');
  }
</script>

<div class="theme-exec yard-root" data-company-map>
  <PageHeader eyebrow="Company" title="System Network Map" subtitle="Departments are regions. Enter IT to see its recorded work flow." />
  <section class="company-map" aria-label="Company departments">
    {#if reading.kind === 'loading'}
      <p role="status">Reading the department registry…</p>
    {:else if reading.kind === 'failed'}
      <p class="load-failed" role="status">The department registry cannot be read — {reading.error}</p>
    {:else}
      <div class="regions">
        {#each reading.data as department (department.code)}
          <article class="department" class:connected={department.code === 'it'} data-department={department.code}>
            {#if department.code === 'it'}
              <a href="/it?view=overview" onclick={open}><h2>{department.label}</h2><span>Open transit map →</span></a>
              <p>Connected worked example</p>
            {:else}
              <h2>{department.label}</h2><p>Not connected</p>
            {/if}
          </article>
        {:else}
          <p>No departments are registered.</p>
        {/each}
      </div>
      <p class="scope">{reading.data.length} registered departments · unconnected regions have no mapped work or health reading.</p>
    {/if}
    <div class="source">
      <span>Source: department registry{readAt === null ? '' : ` · read ${new Date(readAt).toLocaleTimeString()}`}</span>
      <button disabled={reading.kind === 'loading'} onclick={() => void refresh()}>Refresh registry</button>
    </div>
  </section>
</div>

<style>
  .company-map { padding: var(--s4); border: 1px solid var(--map-rule); border-radius: var(--radius); background: var(--map-bg); color: var(--map-ink); }
  .regions { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr)); gap: var(--s4); }
  .department { padding: var(--s4); border: 1px dashed var(--map-rule); border-radius: var(--radius); background: var(--map-surface); }
  .department.connected { border-style: solid; }
  h2 { margin: 0 0 var(--s2); font-size: 20px; overflow-wrap: anywhere; }
  a { display: block; color: var(--map-ink); text-decoration: none; }
  a:hover, a:focus-visible { text-decoration: underline; }
  p, .source { color: var(--map-muted); }
  .scope { margin-top: var(--s4); }
  .source { display: flex; gap: var(--s3); justify-content: space-between; align-items: center; flex-wrap: wrap; font-size: 12px; }
  button { padding: var(--s2); background: var(--map-surface); color: var(--map-ink); border: 1px solid var(--map-rule); border-radius: var(--radius); cursor: pointer; }
  .load-failed { color: var(--map-bad-ink); }
</style>
