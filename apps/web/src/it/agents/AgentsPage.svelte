<script lang="ts">
  // /it/registry/agents — the agents registry as a directory (backlog
  // 62988516). David, 2026-09-26: "Agents can go under a page in the IT
  // department" — never the People roster. What each field is, and why
  // there is no reports-to line, is agents.ts's header; this file only
  // draws it.
  //
  // READING ORDER: one card per registry row — who it is and what it
  // signs as, then the caps the registry declares, then what it holds
  // and what it has finished. The roster is one read; each card's runs
  // and held steps are that card's own reads, so one failing is said
  // on that card and does not blank the directory.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../../router';
  import { costText } from '../crew/crew';
  import {
    budgetText,
    capText,
    effortsRun,
    fetchAgentDetail,
    fetchAgents,
    heldText,
    RUN_WINDOW,
    type Agent,
    type AgentDetail,
  } from './agents';
  import type { Remote } from '../../data/remote';

  let roster = $state<Remote<ReadonlyArray<Agent>>>({ kind: 'loading' });
  let details = $state<Readonly<Record<string, AgentDetail>>>({});

  onMount(() => {
    void (async () => {
      roster = await fetchAgents();
      if (roster.kind !== 'ready') return;
      await Promise.all(
        roster.data.map(async (a) => {
          const d = await fetchAgentDetail(a);
          details = { ...details, [a.id]: d };
        }),
      );
    })();
  });

  const when = (iso: string | null): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : '—');
</script>

<div class="ag-root">
  <PageHeader
    eyebrow="IT · Registry · who runs work besides people"
    title="Agents"
    subtitle="The agents registry: every agent that can hold a step, as the registry declares it. Agents are never People rows and never headcount. Read-only here; the registry is written by boss tenant publish."
  />

  {#if roster.kind === 'loading'}
    <p class="ag-quiet">Reading the agents registry…</p>
  {:else if roster.kind === 'failed'}
    <p class="ag-fail load-failed">
      The agents registry did not answer: {roster.error}. An unreachable read is not an empty registry, so
      nothing is drawn.
    </p>
  {:else if roster.data.length === 0}
    <p class="ag-notice">The registry holds no agent.</p>
  {:else}
    {#each roster.data as a (a.id)}
      {@const d = details[a.id]}
      <section class="ag-card" aria-label={a.name}>
        <h2>{a.name} <span class="mono ag-id">{a.id}</span></h2>
        <dl class="ag-facts">
          <dt>Aliases</dt>
          <dd class="mono">{a.aliases.length === 0 ? 'none' : a.aliases.join(', ')}</dd>
          <dt>Role</dt>
          <dd class="mono">{a.role ?? 'holds no role'}</dd>
          <dt>Department</dt>
          <dd class="mono">{a.department ?? 'not declared'}</dd>
          <dt>Default model</dt>
          <dd class="mono">{a.defaultModel ?? 'not declared'}</dd>
          <dt>Hourly budget</dt>
          <dd class="mono">{budgetText(a.hourlyBudgetUsdMicros)}</dd>
          <dt>Concurrent runs, at most</dt>
          <dd class="mono">{capText(a.maxConcurrentRuns)}</dd>
          <dt>Open steps held</dt>
          <dd class="mono">
            {#if !d}
              reading…
            {:else if d.held.kind === 'failed'}
              <span class="load-failed">did not answer: {d.held.error}</span>
            {:else if d.held.kind === 'ready'}
              {heldText(d.held.data)}
            {/if}
          </dd>
          {#if d && d.runs.kind === 'ready' && d.runs.data.length > 0}
            <dt>Effort in these runs</dt>
            <dd class="mono">{effortsRun(d.runs.data)}</dd>
          {/if}
        </dl>

        {#if !d}
          <p class="ag-quiet">Reading this agent's runs…</p>
        {:else if d.runs.kind === 'failed'}
          <p class="ag-fail load-failed">The agent-run record did not answer: {d.runs.error}.</p>
        {:else if d.runs.kind === 'ready' && d.runs.data.length === 0}
          <p class="ag-quiet">No finished run is recorded for this id.</p>
        {:else if d.runs.kind === 'ready'}
          <h3>
            {d.runs.data.length === RUN_WINDOW
              ? `The newest ${RUN_WINDOW} finished runs`
              : `${d.runs.data.length} finished run${d.runs.data.length === 1 ? '' : 's'}`}
          </h3>
          <div class="ag-tbl">
            <table class="ag-table">
              <thead>
                <tr><th>Finished</th><th>Outcome</th><th>Effort</th><th>Model</th><th class="num">Cost</th><th>Work</th></tr>
              </thead>
              <tbody>
                {#each d.runs.data as r (r.runId)}
                  <tr>
                    <td><EntityLink kind="job" id={r.runId} label={when(r.finishedAt)} mono /></td>
                    <td>{r.outcome ?? 'not recorded'}</td>
                    <td>{r.effort ?? 'not recorded'}</td>
                    <td class="mono">{r.model ?? 'not recorded'}</td>
                    <td class="num mono">{costText(r)}</td>
                    <td>
                      {#if r.packet}
                        <EntityLink kind="job" id={r.packet} label={r.branch ?? 'the packet it ran'} mono />
                      {:else}
                        {r.branch ?? '—'}
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {/each}

    <p class="ag-footnote">
      The registry records no manager for an agent, so no reports-to line is drawn. Open steps held counts the
      ready and active steps assigned to the id or any alias. Runs are finished runs from the agent-run record,
      each linked to its own packet; a run still open is on the Department Map's
      <Link to={href('/it?at=shop-floor')}>shop floor</Link>.
    </p>
  {/if}
</div>

<style>
  .ag-root { padding: 0 32px 32px; }
  .ag-quiet { color: var(--static); font-size: 13px; }
  .ag-fail {
    color: var(--warn); border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .ag-notice {
    color: var(--fog); border: 1px solid var(--hairline);
    padding: 8px 12px; font-size: 13px; max-width: 90ch;
  }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .ag-card { border: 1px solid var(--hairline); padding: 12px 16px; margin-top: 16px; }
  .ag-card h2 { font-size: 16px; font-weight: 600; margin: 0 0 8px; }
  .ag-card h3 { font-size: 13px; font-weight: 500; color: var(--static); margin: 14px 0 4px; }
  .ag-id { font-size: 12px; font-weight: 400; color: var(--static); margin-left: 6px; }
  .ag-facts {
    display: grid; grid-template-columns: max-content 1fr;
    gap: 4px 16px; margin: 0; font-size: 13px;
  }
  .ag-facts dt { color: var(--static); }
  .ag-facts dd { margin: 0; overflow-wrap: anywhere; }
  .ag-tbl { overflow-x: auto; }
  .ag-table { width: 100%; border-collapse: collapse; font-size: 12px; }
  .ag-table th { text-align: left; font-weight: 500; color: var(--static); padding: 4px 8px; border-bottom: 1px solid var(--hairline); }
  .ag-table td { padding: 6px 8px; border-bottom: 1px solid var(--hairline); white-space: nowrap; }
  .ag-table .num { text-align: right; }
  .ag-footnote { color: var(--text-faint); font-size: 12px; max-width: 90ch; margin-top: 20px; }
</style>
