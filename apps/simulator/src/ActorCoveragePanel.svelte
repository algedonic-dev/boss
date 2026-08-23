<script lang="ts">
  // Actor coverage — is the sim driving the WHOLE roster? Renders the
  // telemetry's `actor_coverage` block: headline totals, the dormant
  // roles named prominently, and the full per-role table. The point is
  // that an under-simulated brewery is VISIBLE on the cockpit's face
  // instead of being rediscovered by SQL. Ordering/labeling logic lives
  // in ./actor-coverage (unit-tested); this component only renders.
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { dormantRoles, sortRoles, statusLabel } from './actor-coverage';
  import type { ActorCoverage } from './types';

  let { coverage }: Readonly<{ coverage: ActorCoverage | undefined }> = $props();

  let rows = $derived(coverage ? sortRoles(coverage.roles) : []);
  let dormant = $derived(coverage ? dormantRoles(coverage.roles) : []);
  // Simulatable = total minus the by-design operator exclusions; the
  // denominator dormancy is judged against.
  let simRoles = $derived(coverage ? coverage.roles_total - coverage.roles_operator : 0);
  let simEmployees = $derived(
    coverage ? coverage.employees_total - coverage.employees_operator : 0,
  );
</script>

<Section title="Actor coverage" wide>
  <p class="point-sub">
    Who on the roster has actually acted — distinct employees per role that completed ≥ 1 step
    this daemon lifetime. A dormant role means no live workflow step routes work to it.
  </p>
  {#if !coverage}
    <p class="status">No coverage telemetry yet (daemon predates actor coverage).</p>
  {:else}
    <div class="cov-headline">
      <div class="stat">
        <span class="stat-num">{coverage.roles_acting}</span>
        <span class="stat-label">of {simRoles} roles acting</span>
      </div>
      <div class="stat">
        <span class="stat-num">{coverage.employees_acting.toLocaleString()}</span>
        <span class="stat-label">of {simEmployees.toLocaleString()} employees acting</span>
      </div>
      <div class="stat" class:alarm={coverage.roles_dormant > 0}>
        <span class="stat-num">{coverage.roles_dormant}</span>
        <span class="stat-label">dormant roles</span>
      </div>
    </div>

    {#if dormant.length > 0}
      <div class="dormant-strip">
        <span class="dormant-title">Dormant — never completed a step:</span>
        {#each dormant as r (r)}
          <span class="dormant-role">{r}</span>
        {/each}
      </div>
    {/if}

    <table class="cov-table">
      <thead>
        <tr>
          <th class="t-role">Role</th>
          <th class="t-num">Roster</th>
          <th class="t-num">Acting</th>
          <th class="t-num">Completions</th>
          <th class="t-status"></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.role)}
          <tr class:is-dormant={r.status === 'dormant'} class:is-operator={r.status === 'operator'}>
            <td class="t-role">{r.role}</td>
            <td class="t-num">{r.roster.toLocaleString()}</td>
            <td class="t-num">{r.acting.toLocaleString()}</td>
            <td class="t-num">{r.completions.toLocaleString()}</td>
            <td class="t-status">
              {#if r.status !== 'acting'}
                <span class="badge-{r.status}">{statusLabel(r.status)}</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if coverage.roles_operator > 0}
      <p class="cov-foot">
        Operator-held roles are excluded from sim driving by design — their steps wait for the
        real human.
      </p>
    {/if}
  {/if}
</Section>

<style>
  .point-sub {
    margin: 0 0 10px;
    font-size: 0.78rem;
    color: #7a6855;
  }
  .cov-headline {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 28px;
    margin-bottom: 12px;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .stat-num {
    font-family: var(--font-display);
    font-size: 2rem;
    font-weight: 700;
    color: var(--brew-malt-dark);
    line-height: 1;
  }
  .stat-label {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--brew-malt);
  }
  .stat.alarm .stat-num,
  .stat.alarm .stat-label {
    color: #8b2b1f;
  }
  .dormant-strip {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    padding: 8px 10px;
    margin-bottom: 12px;
    background: rgba(139, 43, 31, 0.07);
    border: 1px solid rgba(139, 43, 31, 0.25);
    border-radius: 6px;
  }
  .dormant-title {
    font-size: 0.78rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: #8b2b1f;
  }
  .dormant-role {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
    color: #8b2b1f;
    background: rgba(139, 43, 31, 0.1);
    padding: 0 0.45em;
    border-radius: 3px;
  }
  .cov-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  .cov-table th {
    text-align: left;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--brew-malt);
    border-bottom: 1px solid #e6d2a8;
    padding: 2px 8px 4px;
  }
  .cov-table td {
    padding: 3px 8px;
    border-bottom: 1px solid #f0e6cf;
  }
  .cov-table tbody tr:nth-child(odd) {
    background: rgba(217, 155, 58, 0.06);
  }
  .t-role {
    color: var(--brew-malt-dark);
    font-weight: 600;
  }
  .t-num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  th.t-num {
    text-align: right;
  }
  .t-status {
    text-align: right;
    white-space: nowrap;
  }
  tr.is-dormant .t-role,
  tr.is-dormant .t-num {
    color: #8b2b1f;
  }
  tr.is-operator .t-role,
  tr.is-operator .t-num {
    color: #a8a29e;
    font-weight: 400;
  }
  .badge-dormant {
    font-size: 0.72rem;
    font-weight: 600;
    color: #8b2b1f;
    background: rgba(139, 43, 31, 0.1);
    border: 1px solid rgba(139, 43, 31, 0.3);
    border-radius: 4px;
    padding: 0 6px;
  }
  .badge-operator {
    font-size: 0.72rem;
    color: #7a6855;
    background: #f5f0e8;
    border: 1px solid #e6dcc8;
    border-radius: 4px;
    padding: 0 6px;
  }
  .cov-foot {
    margin: 8px 0 0;
    font-size: 0.74rem;
    color: #a8957a;
  }
  .status {
    margin: 0 0 6px;
    color: #7a6855;
    font-style: italic;
  }
</style>
