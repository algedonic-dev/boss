<script lang="ts">
  import { onMount } from 'svelte';
  import { href } from '../../router';
  import { projectRoster, readRoster, type Capture } from './native-roster';
  let captures = $state<ReadonlyArray<Capture> | null>(null);
  let error = $state<string | null>(null);
  let observedNow = $state(new Date().toISOString());
  const observations = $derived(captures === null ? [] : projectRoster(captures, observedNow));
  async function refresh(): Promise<void> {
    try {
      captures = await readRoster(async (url) => {
        const response = await fetch(url);
        if (!response.ok) throw new Error('The signed roster read did not answer');
        return response.json() as Promise<unknown>;
      });
      error = null;
    } catch { error = 'Roster evidence unreadable or incomplete; runtime state unknown.'; }
    observedNow = new Date().toISOString();
  }
  onMount(() => {
    void refresh();
    const timer = setInterval(() => { observedNow = new Date().toISOString(); }, 10_000);
    return () => clearInterval(timer);
  });
</script>

<section aria-label="Observed native runtime roster">
  <h3>Observed native runtime</h3>
  <p>Explicit coordinator snapshots. Five-minute expiry; no continuous collection or heartbeat. Workflow assignments are separate.</p>
  <button onclick={() => void refresh()}>Read recorded snapshots</button>
  {#if error}
    <p role="alert">{error}</p>
  {:else if captures === null}
    <p>Reading recorded observations…</p>
  {:else if observations.length === 0}
    <p>No snapshot visible to this reader. Runtime state unknown.</p>
  {:else}
    {#each observations as snapshot (snapshot.namespace)}
      <article>
        <p>Native namespace {snapshot.namespace} · {snapshot.state}</p>
        <p>{snapshot.source} · collected {snapshot.from} to {snapshot.to} · recorder {snapshot.observer}</p>
        <a href={href(`/ux/jobs/${snapshot.captureId}`)}>BOSS capture receipt</a>
        {#if snapshot.state === 'unknown'}
          <p>Latest capture failed or has future evidence; runtime state unknown. Last complete snapshot: {snapshot.lastComplete?.to ?? 'unavailable'}.</p>
        {:else if snapshot.rows.length === 0}
          <p>No agents in this complete snapshot of this namespace.</p>
        {:else}
          <ul>
            {#each snapshot.rows as row (row.path)}
              <li>{row.path} · {snapshot.state === 'stale' ? 'stale observation' : `observed ${row.status}`} · current registered task unknown</li>
            {/each}
          </ul>
        {/if}
      </article>
    {/each}
  {/if}
  <p>Paths are reused aliases in the recorded namespace. Native agent ID, current task, observed model and individual billing are unavailable. A finished executor does not establish delivery or gate success.</p>
</section>

<style>
  section { border: 1px solid var(--border); padding: 1rem; margin: 1rem 0; max-width: 68rem; }
  p, li { font-size: 0.78rem; overflow-wrap: anywhere; }
  article { border-top: 1px solid var(--border); margin-top: 0.75rem; padding-top: 0.5rem; }
</style>
