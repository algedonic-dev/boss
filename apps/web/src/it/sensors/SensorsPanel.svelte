<script lang="ts">
  import { onMount } from 'svelte';
  import { readSensors, groupSensors, type SensorsView } from './sensors';
  import { fetchRemote } from '../../data/remote';

  type Reading = { kind: 'loading' } | { kind: 'ready'; data: SensorsView } | { kind: 'failed'; error: string };
  let reading = $state<Reading>({ kind: 'loading' });
  let disposed = false;
  async function refresh() {
    reading = { kind: 'loading' };
    try {
      const data = await readSensors(async (url) => {
        const result = await fetchRemote(url, (raw) => raw);
        if (result.kind === 'failed') throw new Error(result.error);
        return result.data;
      }, new Date().toISOString());
      if (!disposed) reading = { kind: 'ready', data };
    } catch (e) {
      if (!disposed) reading = { kind: 'failed', error: e instanceof Error ? e.message : String(e) };
    }
  }
  onMount(() => { void refresh(); return () => { disposed = true; }; });
</script>

<section aria-label="Sensor readings" class="sensor-panel">
  <div class="sensor-heading"><h3>Arriving through sensors</h3><button onclick={refresh} disabled={reading.kind === 'loading'}>Refresh readings</button></div>
  <p>Leaving: no outbound transport. Department inbox routing is declared by sensor rows; this panel shows their existing readings.</p>
  {#if reading.kind === 'loading'}
    <p>Reading the sensor registry…</p>
  {:else if reading.kind === 'failed'}
    <p role="alert">Sensor readings unavailable: {reading.error}</p>
  {:else}
    <p>Window: {reading.data.since} to {reading.data.until}. Observation ends at the refresh time.</p>
    {#if reading.data.rows.length === 0}<p>No sensors declared in the registry.</p>{/if}
    {#each groupSensors(reading.data.rows) as group (group.credential)}
      <h4>{group.credential === '' ? 'Sources without a credential' : group.credential}</h4>
      <div class="sensor-scroll"><table>
        <thead><tr><th>Sensor / source</th><th>Selector / protocol</th><th>Last poll</th><th>Arrived</th><th>Stamped</th><th>Unstamped</th><th>Packets opened</th></tr></thead>
        <tbody>{#each group.rows as row (row.sensor.id)}
          <tr><td>{row.sensor.id}<br />{row.sensor.source}{row.sensor.enabled ? '' : ' · disabled'}</td>
            <td>{row.sensor.selector ?? 'No selector'}<br />{row.sensor.opens_kind}</td>
            <td>{row.sensor.last_polled_at ?? (row.sensor.every_minutes === 0 ? 'Never polled (push source)' : 'No poll recorded')}</td>
            {#if row.readings}
              <td>{row.readings.arrived}</td><td>{row.readings.stamped}</td><td>{row.readings.unstamped}</td>
              <td>{#each row.readings.packets as packet (packet)}<a href={`/ux/jobs/${encodeURIComponent(packet)}`}>{packet}</a> {/each}</td>
            {:else}<td colspan="4">Readings unavailable: {row.error}</td>{/if}
          </tr>
        {/each}</tbody>
      </table></div>
    {/each}
  {/if}
</section>

<style>
  .sensor-heading { display: flex; align-items: center; justify-content: space-between; gap: var(--s2); }
  .sensor-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: var(--s2); text-align: left; vertical-align: top; border-bottom: 1px solid var(--border); }
  h4 { margin-top: var(--s4); }
</style>
